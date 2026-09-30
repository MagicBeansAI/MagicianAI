//! Source-level architecture tripwires for ordinary-stack agentic execution.
//!
//! These assertions complement runtime tests: a future route can compile and
//! still accidentally construct a large agentic future on an Actix worker.
//! Keeping the production launch sites in one table makes that review surface
//! explicit and fails the normal suite when a boundary is silently removed.

const EXECUTOR: &str = include_str!("../src/magician_v2/execution/agentic/executor.rs");
const APPLY_PHASE: &str =
    include_str!("../src/magician_v2/execution/agentic/run_loop/phases/apply.rs");
const DELEGATION_DISPATCH: &str =
    include_str!("../src/magician_v2/execution/agentic/delegation_dispatch.rs");
const ORCHESTRATOR: &str = include_str!("../src/magician_v2/orchestrator/v2_orchestrator.rs");
const ARTIFACT_SERVICE: &str = include_str!("../src/magician_v2/artifact_v2/service.rs");
const AGENT_RUNTIME: &str = include_str!("../src/magician_v2/agents/runtime.rs");
const WEB_API: &str = include_str!("../../magician-api/src/web_api.rs");
const EVAL_RUNNER: &str = include_str!("../../magician-surfaces/src/evals/runner.rs");
const CHAT_SERVICE: &str = include_str!("../src/magician_v2/chat/service.rs");
const CHAT_API: &str = include_str!("../../magician-api/src/chat_api.rs");
const CONTEXTUAL_WRITING_API: &str =
    include_str!("../../magician-api/src/contextual_writing_api.rs");
const MEDIA_API: &str = include_str!("../../magician-api/src/media_api.rs");
const VOICE_ORCHESTRATOR: &str =
    include_str!("../../magician-media/src/media_rails/voice_orchestrator.rs");
const EXECUTION_RUNTIME: &str = include_str!("../src/magician_v2/execution/runtime_boundary.rs");
const MAGIOS_TUTOR_VIEW_MODEL: &str =
    include_str!("../../magios/Magios/TutorOverlayViewModel.swift");
const VOICE_CONTROL: &str = include_str!("../../magician-api/src/voice_control_handler.rs");
const SUPERVISOR_SCRIPT: &str = include_str!("../../scripts/run-supervisor.sh");
const RUST_TEST_SCRIPT: &str = include_str!("../../scripts/run-rust-tests-with-report.sh");

struct BoundaryContract {
    label: &'static str,
    source: &'static str,
    start: &'static str,
    end: &'static str,
    required: &'static [&'static str],
}

fn source_section<'a>(contract: &BoundaryContract, source: &'a str) -> &'a str {
    let start = source.find(contract.start).unwrap_or_else(|| {
        panic!(
            "{}: missing start marker `{}`",
            contract.label, contract.start
        )
    });
    let tail = &source[start..];
    let end = tail
        .find(contract.end)
        .filter(|offset| *offset > 0)
        .unwrap_or_else(|| panic!("{}: missing end marker `{}`", contract.label, contract.end));
    &tail[..end]
}

#[test]
fn production_agentic_entry_points_retain_scheduler_root_boundaries() {
    let contracts = [
        BoundaryContract {
            label: "embedded/public agentic core",
            source: EXECUTOR,
            start: "async fn execute_agentically_scoped(",
            end: "async fn execute_agentically_inner(",
            required: &[
                "runtime_boundary::run_execution_job",
                "execute_agentically_scoped_on_execution_runtime(",
                "SchedulerRootLane::new",
                "scheduler_root_worker.run()",
                "spawn_with_execution_token_meter_in_set",
                "warm_provider_sanitizer",
                "embedded_compiled_pack_defs_ref",
            ],
        },
        BoundaryContract {
            label: "scoped task context hydration",
            source: EXECUTOR,
            start: "async fn hydrate_prompt_context_after_owner_transition_with_cancellation(",
            end: "fn handover_target_previously_failed(",
            required: &[
                "let retrieval_job = Box::pin(async move",
                "spawn_with_execution_token_meter_in_set(&mut retrieval_jobs, retrieval_job)",
            ],
        },
        BoundaryContract {
            label: "direct action preflight segmentation",
            source: EXECUTOR,
            start: "fn authorize_direct_action_preflight<'a>(",
            end: "fn execute_direct_path_core_body<'a>(",
            required: &[
                "fn file_sandbox_direct_action_preflight<'a>(",
                "fn bash_sandbox_direct_action_preflight<'a>(",
                "Pin<Box<dyn Future<Output = Result<Option<ExecutePathControl>>> + Send + 'a>>",
                "Box::pin(async move",
            ],
        },
        BoundaryContract {
            label: "direct action preflight orchestration",
            source: EXECUTOR,
            start: "fn execute_direct_path_core_body<'a>(",
            end: "/// P0.4: intercept an `ExecutionError::PathAccessDenied`",
            required: &[
                "authorize_direct_action_preflight(",
                "file_sandbox_direct_action_preflight(",
                "bash_sandbox_direct_action_preflight(",
                "execute_direct_path_post_setup_tail(",
                "fn execute_direct_path_on_scheduler_root<'a>(",
                "ScheduledDirectPathState",
                "\"complete direct path\"",
            ],
        },
        BoundaryContract {
            label: "direct action and state construction",
            source: EXECUTOR,
            start: "fn execute_direct_path_post_setup_tail<'a>(",
            end: "fn delegate_outcome_primary_text(outcome: &AgenticOutcome)",
            required: &[
                "schedule_action_execution_job",
                "schedule_state_build_job",
                "schedule_autonomous_projection_failure_output",
            ],
        },
        BoundaryContract {
            label: "durable continuation",
            source: EXECUTOR,
            start: "async fn execute_with_durable_continuations(",
            end: "async fn execute_with_durable_continuations_inner(",
            required: &[
                "runtime_boundary::spawn_execution_job",
                "move || async move",
            ],
        },
        BoundaryContract {
            label: "exact paused resume",
            source: ORCHESTRATOR,
            start: "async fn spawn_exact_paused_resume(",
            end: "pub async fn resume_execution_tree(",
            required: &[
                "runtime_boundary::spawn_execution_job",
                "execute_agentically_resume_exact(",
            ],
        },
        BoundaryContract {
            label: "task-backed status outcome projection",
            source: ARTIFACT_SERVICE,
            start: "pub fn persist_runtime_execution_outcome_by_execution_id<'a>(",
            end: "/// Reactively finalize an ORPHANED user-pause",
            required: &[
                "runtime_boundary::run_execution_job",
                "CapturedExecutionTokenMeter::current()",
                ".scope(",
                "resolve_scope_for_execution_id(",
                "persist_runtime_execution_outcome(",
            ],
        },
        BoundaryContract {
            label: "caller-supplied task outcome projection",
            source: ARTIFACT_SERVICE,
            start: "pub async fn persist_external_execution_outcome(",
            end: "/// Project one runtime outcome into Artifact V2",
            required: &[
                "runtime_boundary::run_execution_job",
                "CapturedExecutionTokenMeter::current()",
                ".scope(",
                "persist_external_execution_outcome_inner(",
            ],
        },
        BoundaryContract {
            label: "task execution",
            source: ARTIFACT_SERVICE,
            start: "pub async fn start_execution(",
            end: "/// Durably accept one concrete schedule occurrence.",
            required: &[
                "runtime_boundary::spawn_execution_job",
                "run_execution(binding)",
            ],
        },
        BoundaryContract {
            label: "scheduled task execution",
            source: ARTIFACT_SERVICE,
            start: "pub async fn launch_accepted_scheduled_execution(",
            end: "async fn resolve_scope_for_execution_id(",
            required: &[
                "runtime_boundary::spawn_execution_job",
                "run_execution(binding)",
            ],
        },
        BoundaryContract {
            label: "terminal output finalizers",
            source: ARTIFACT_SERVICE,
            start: "async fn run_execution_finalizer_on_execution_runtime(",
            end: "pub trait TaskFeedProjector",
            required: &[
                "runtime_boundary::run_execution_job",
                "async fn run_task_agent_finalizer_on_execution_runtime(",
                "async fn run_task_user_finalizer_on_execution_runtime(",
                "async fn run_task_user_projection_on_execution_runtime(",
            ],
        },
        BoundaryContract {
            label: "delegated parent continuation",
            source: ARTIFACT_SERVICE,
            start: "fn maybe_resume_parent_after_v3_child_results<'a>(",
            end: "async fn binding_for_existing_execution(",
            required: &[
                "dyn std::future::Future<Output = Result<bool, ArtifactV2Error>>",
                "+ Send",
                "Box::pin(async move",
                "runtime_boundary::spawn_execution_job",
                "move || async move",
                "continue_execution_with_orchestrator(",
            ],
        },
        BoundaryContract {
            label: "child-terminal parent continuation",
            source: AGENT_RUNTIME,
            start: "async fn notify_parent_after_forced_child_terminal(",
            end: "/// Phase 2 (diff-approval delegation)",
            required: &[
                "runtime_boundary::spawn_execution_job",
                "move || async move",
                "reconcile_waiting_children_and_continue(",
            ],
        },
        BoundaryContract {
            label: "inline capability-pack execution",
            source: ARTIFACT_SERVICE,
            start: "pub async fn run_chat_pack_inline(",
            end: "async fn run_chat_pack_inline_on_execution_runtime(",
            required: &[
                "runtime_boundary::spawn_execution_job",
                "run_chat_pack_inline_on_execution_runtime(",
            ],
        },
        BoundaryContract {
            label: "manual agent trigger",
            source: WEB_API,
            start: "fn dispatch_manual_trigger_cycle(",
            end: "async fn cancel_execution_with_retries(",
            required: &["spawn_execution_job", "execute_agentic_direct_with_outcome"],
        },
        BoundaryContract {
            label: "validated HTTP resume",
            source: WEB_API,
            start: "async fn resume_agentic_execution_with_scope_inner(",
            end: "pub async fn continue_agentic_execution(",
            required: &[
                "spawn_execution_job",
                "execute_agentically_resume_with_validation(",
            ],
        },
        BoundaryContract {
            label: "continued HTTP execution",
            source: WEB_API,
            start: "pub async fn continue_agentic_execution(",
            end: "pub async fn cancel_paused_execution(",
            required: &["spawn_execution_job", "execute_agentically_continue("],
        },
        BoundaryContract {
            label: "eval lane",
            source: EVAL_RUNNER,
            start: "pub async fn start_lane(",
            end: "pub async fn begin_lane(",
            required: &["spawn_execution_job"],
        },
        BoundaryContract {
            label: "cascaded hands-free chat turn",
            source: VOICE_CONTROL,
            start: "fn handle_finalized_user_transcript(",
            end: "fn on_session_end(",
            required: &[
                "spawn_execution_job(move || async move",
                "process_cascaded_voice_turn(&text)",
            ],
        },
        BoundaryContract {
            label: "voice tutor and app-copilot takeover",
            source: VOICE_CONTROL,
            start: "fn handle_tutor_takeover_transcript(",
            end: "fn remember_completed_tutor_takeover_key(",
            required: &[
                "spawn_execution_job(move || async move",
                ".submit_tutor_takeover_turn(",
            ],
        },
    ];

    for contract in contracts {
        let section = source_section(&contract, contract.source);
        for required in contract.required {
            assert!(
                section.contains(required),
                "{} must retain `{required}`",
                contract.label
            );
        }
    }

    // Both autonomous roots and delegated children must cross the same shared
    // runtime. Their enclosing functions are intentionally large, so exact
    // stable launch fragments are less brittle than function-end slicing.
    assert!(
        AGENT_RUNTIME
            .match_indices("spawn_execution_job(move || async move")
            .count()
            >= 2,
        "agent runtime must isolate both root cycles and delegated children"
    );

    let direct_tail = EXECUTOR
        .split_once("fn execute_direct_path_post_setup_tail<'a>(")
        .expect("direct path tail")
        .1
        .split_once("fn delegate_outcome_primary_text(outcome: &AgenticOutcome)")
        .expect("direct path tail end")
        .0;
    assert!(
        !direct_tail.contains("tokio::spawn")
            && !direct_tail.contains("tokio::task::JoinSet::new()"),
        "deep direct-path code must submit lazy jobs to the pre-spawned lane, not create Tokio tasks"
    );
    assert!(
        EXECUTOR.contains("type SchedulerRootTaskFactory =")
            && EXECUTOR.contains("UnboundedSender<SchedulerRootTaskFactory>"),
        "the execution-local lane must transport lazy task factories"
    );
    assert!(
        ARTIFACT_SERVICE
            .match_indices("run_execution_finalizer_on_execution_runtime(")
            .count()
            >= 3,
        "both child and root execution-output synthesis must use the fresh terminal-finalizer task root"
    );
    assert!(
        !ARTIFACT_SERVICE.contains("self.execution_finalizer.finalize(")
            && !ARTIFACT_SERVICE.contains("self.task_agent_finalizer.finalize(")
            && !ARTIFACT_SERVICE.contains("self.task_user_finalizer.finalize("),
        "model-backed output finalizers must not be polled inline on an agentic terminal stack"
    );
    assert!(
        EXECUTOR.contains("let mut tasks = tokio::task::JoinSet::new();")
            && EXECUTOR.contains("let task = task_factory();")
            && EXECUTOR.contains("spawn_with_execution_token_meter_in_set(&mut tasks, task)"),
        "broad child futures must be constructed lazily as structured scheduler-root tasks"
    );
    assert!(
        !EXECUTOR.contains("task_factory().await"),
        "the lane worker must remain available for nested action/state jobs"
    );
    let delegate_handler = EXECUTOR
        .split_once("async fn handle_delegate_to_agent_decision(")
        .expect("delegation decision handler")
        .1
        .split_once("fn schedule_delegation_spawn_job(")
        .expect("delegation scheduler-root wrapper")
        .0;
    assert!(
        delegate_handler.contains("schedule_delegation_spawn_job(")
            && !delegate_handler.contains(".spawn_children("),
        "the deep delegation decision handler must submit admission to its pre-spawned lane"
    );
    let delegate_scheduler = EXECUTOR
        .split_once("fn schedule_delegation_spawn_job(")
        .expect("delegation scheduler-root wrapper")
        .1
        .split_once("fn sub_goal_paraphrases_parent(")
        .expect("delegation scheduler-root wrapper end")
        .0;
    assert!(
        delegate_scheduler.contains("scheduler_root_lane.schedule(")
            && delegate_scheduler.contains("dispatcher.spawn_children("),
        "delegation admission must be lazily constructed on the execution-local lane"
    );
    let external_delegation_boundary = DELEGATION_DISPATCH
        // Visibility-agnostic: the crate split widened this from `pub(crate)`
        // to `pub` so `magician-api`/`magician-bin` could reach it.
        .split_once("async fn spawn_children_on_execution_runtime(")
        .expect("external delegation runtime boundary")
        .1;
    assert!(
        external_delegation_boundary.contains("runtime_boundary::run_execution_job")
            && external_delegation_boundary.contains("CapturedExecutionTokenMeter::current")
            && external_delegation_boundary.contains("dispatcher.spawn_children("),
        "non-executor delegation callers must retain the joined execution-runtime boundary"
    );
    assert!(
        ORCHESTRATOR.contains("spawn_children_on_execution_runtime(")
            && ARTIFACT_SERVICE.contains("spawn_children_on_execution_runtime("),
        "explicit delegation and verification repair must use the canonical runtime wrapper"
    );
    assert_eq!(
        EXECUTOR.match_indices("dispatcher.spawn_children(").count(),
        1,
        "the executor may call raw delegation dispatch only inside its lane wrapper"
    );
    assert_eq!(
        DELEGATION_DISPATCH
            .match_indices("dispatcher.spawn_children(")
            .count(),
        1,
        "the delegation module may call raw dispatch only inside its runtime wrapper"
    );
    let owner_transition = EXECUTOR
        .split_once("async fn prepare_owner_transition_at_scheduler_root(")
        .expect("owner-transition scheduler-root preparation")
        .1
        .split_once("// ============================================================================\n// Loop Pressure Context")
        .expect("owner-transition scheduler-root preparation end")
        .0;
    assert!(
        owner_transition.contains("run_on_agentic_scheduler_root(")
            && owner_transition.contains("prepare_owner_transition_at_scheduler_root("),
        "handover, yield-back, and continuation owner transitions must isolate profile/storage work"
    );
    let terminal_precision = EXECUTOR
        .split_once("async fn review_terminal_draft_against_opened_evidence(")
        .expect("terminal precision review")
        .1
        .split_once("fn goal_reached_rejection_reason(")
        .expect("terminal precision review end")
        .0;
    assert!(
        terminal_precision.contains("run_terminal_precision_review_at_scheduler_root(")
            && terminal_precision.contains("run_on_agentic_scheduler_root("),
        "terminal model-backed precision review must not poll inline below the agent loop"
    );
    let durable_task_state = EXECUTOR
        .split_once("struct DurableTaskStateLoadRequest")
        .expect("durable task-state boundary")
        .1
        .split_once("fn close_status_from_task_state_action(")
        .expect("durable task-state boundary end")
        .0;
    assert!(
        durable_task_state.contains("run_on_agentic_scheduler_root(")
            && durable_task_state.contains("load_persisted_durable_task_state_inner")
            && durable_task_state.contains("persist_durable_task_state_inner"),
        "durable task-state serde and provider I/O must stay behind the shared scheduler-root helper"
    );
    let shared_root_helper = EXECUTOR
        .split_once("async fn run_on_agentic_scheduler_root")
        .expect("shared agentic scheduler-root helper")
        .1
        .split_once("async fn load_persisted_durable_task_state(")
        .expect("shared agentic scheduler-root helper end")
        .0;
    assert!(
        shared_root_helper.contains("scheduler_root_lane.schedule(")
            && shared_root_helper.contains("runtime_boundary::run_execution_job")
            && shared_root_helper.contains("CapturedExecutionTokenMeter::current"),
        "heavy executor seams must share one context-preserving lane/runtime boundary"
    );
    for (label, marker) in [
        ("initial owner load", "\"initial owner profile load\""),
        ("owner refresh", "\"owner profile refresh\""),
        (
            "terminal summary",
            "\"terminal execution-summary persistence\"",
        ),
        ("paused summary", "\"paused execution-summary persistence\""),
        (
            "partial paused summary",
            "\"partial paused execution-summary persistence\"",
        ),
    ] {
        assert!(
            EXECUTOR.contains(marker),
            "{label} must remain behind the shared scheduler-root helper"
        );
    }
    // The run-loop extraction moved the production callers out of executor.rs
    // and into Apply. Keep this tripwire on the callers: executor.rs is allowed
    // one raw core-body call inside the scheduler-root wrapper itself, while
    // Apply must enter through that wrapper at each of its three call sites.
    let production_apply = APPLY_PHASE
        .split_once("\n#[cfg(test)]\nmod gate_tests")
        .map(|(production, _)| production)
        .expect("Apply production/test boundary");
    assert_eq!(
        production_apply
            .match_indices("execute_direct_path_on_scheduler_root(")
            .count(),
        3,
        "Apply's three direct-action callers must retain the scheduler-root wrapper"
    );
    assert!(
        !production_apply.contains("execute_direct_path_core_body("),
        "Apply must not bypass the scheduler-root wrapper with a raw direct-path call"
    );
}

#[test]
fn exact_pause_body_hydration_stays_off_request_and_orchestration_workers() {
    let store_boundary = EXECUTOR
        .split_once("pub async fn take_serializable_by_key_from_disk_on_execution_runtime(")
        .expect("exact pause hydration boundary")
        .1
        .split_once("/// Clear all stored pause data")
        .expect("exact pause hydration boundary end")
        .0;
    assert!(store_boundary.contains("runtime_boundary::run_execution_job"));
    assert!(store_boundary.contains("store.take_serializable_by_key_from_disk_scoped("));

    let web_recovery = WEB_API
        .split_once("async fn take_pause_by_storage_key_with_recovery(")
        .expect("HTTP pause recovery")
        .1
        .split_once("async fn take_pause_for_request(")
        .expect("HTTP pause recovery end")
        .0;
    assert!(web_recovery.contains("take_serializable_by_key_from_disk_scoped_on_execution_runtime"));
    assert!(web_recovery.contains(".await"));

    let orchestrator_recovery = ORCHESTRATOR
        .split_once("async fn take_manual_pause_for_execution_with_recovery(")
        .expect("orchestrator pause recovery")
        .1
        .split_once("fn restore_manual_checkpoint_durable(")
        .expect("orchestrator pause recovery end")
        .0;
    assert!(
        orchestrator_recovery.contains("take_serializable_by_key_from_disk_on_execution_runtime")
    );
    assert!(orchestrator_recovery.contains(".await"));
}

#[test]
fn canonical_launch_and_test_scripts_do_not_mask_stack_growth() {
    for (label, script) in [
        ("supervisor", SUPERVISOR_SCRIPT),
        ("Rust test runner", RUST_TEST_SCRIPT),
    ] {
        assert!(
            script.contains("unset RUST_MIN_STACK"),
            "{label} must unset RUST_MIN_STACK"
        );
        assert!(
            !script.contains("RUST_MIN_STACK:=8388608")
                && !script.contains("RUST_MIN_STACK=8388608"),
            "{label} must not restore the retired 8 MiB default"
        );
    }
    assert!(SUPERVISOR_SCRIPT.contains("MAGICIAN_EMERGENCY_RUST_MIN_STACK"));

    let runtime_builder = EXECUTION_RUNTIME
        .split_once("pub fn build_execution_runtime")
        .expect("execution runtime builder")
        .1
        .split_once("pub fn set_execution_runtime_handle")
        .expect("execution runtime registration")
        .0;
    assert!(
        !runtime_builder.contains("thread_stack_size") && !runtime_builder.contains("stack_size"),
        "the execution runtime must use Tokio's ordinary worker-stack policy"
    );
    assert!(
        !EXECUTION_RUNTIME.contains("EXECUTION_WORKER_STACK_BYTES"),
        "the retired enlarged execution-stack constant must not return"
    );
}

#[test]
fn chat_and_tutor_entry_points_retain_scheduler_root_boundaries() {
    assert!(
        MAGIOS_TUTOR_VIEW_MODEL.contains("chat/sessions/\\(sessionId)/messages"),
        "native Tutor route changed; review its backend stack boundary"
    );
    assert!(MAGIOS_TUTOR_VIEW_MODEL.contains("\"source_surface\": \"ios_tutor_overlay\""));
    assert!(
        !CHAT_SERVICE.contains("pub async fn process_message(\n"),
        "the raw current-runtime Chat entry point must remain private"
    );
    assert!(
        !CHAT_SERVICE.contains("pub async fn process_message_streaming(\n"),
        "the raw current-runtime streaming entry point must remain private"
    );
    assert!(
        !CHAT_SERVICE.contains("async fn process_message_streaming(\n"),
        "a raw Ask-only streaming wrapper would rebuild the large turn future outside the production boundary"
    );

    let production_chat = CHAT_SERVICE
        .find("\n#[cfg(test)]\nmod tests")
        .into_iter()
        .chain(CHAT_SERVICE.find("\n#[cfg(any(test"))
        .min()
        .map(|at| &CHAT_SERVICE[..at])
        .expect("chat production/test boundary");
    let inline_turn_calls = production_chat
        .match_indices(".process_chat_inline_turn(")
        .count();
    let boxed_inline_turn_calls = production_chat
        .match_indices("Box::pin(self.process_chat_inline_turn(")
        .count();
    assert_eq!(
        inline_turn_calls, boxed_inline_turn_calls,
        "every production inline-turn caller must heap-erase the nested future"
    );
    let prompt_render_calls = production_chat
        .match_indices(".render_chat_outer_loop_prompt(")
        .count();
    let boxed_prompt_render_calls = production_chat
        .match_indices("Box::pin(self.render_chat_outer_loop_prompt(")
        .count();
    assert_eq!(
        prompt_render_calls, boxed_prompt_render_calls,
        "every production outer-loop prompt caller must heap-erase the nested future"
    );

    let contracts = [
        BoundaryContract {
            label: "mode-aware non-streaming chat runtime transfer",
            source: CHAT_SERVICE,
            start: "pub async fn process_message_with_mode_on_execution_runtime(",
            end: "async fn process_message_with_mode(",
            required: &[
                "run_execution_job(move || async move",
                ".process_message_with_mode(",
            ],
        },
        BoundaryContract {
            label: "streaming chat runtime transfer",
            source: CHAT_SERVICE,
            start: "pub async fn process_message_streaming_with_mode_on_execution_runtime(",
            end: "fn process_message_streaming_with_mode<'a>(",
            required: &[
                "run_execution_job(move || async move",
                ".process_message_streaming_with_mode(",
            ],
        },
        BoundaryContract {
            label: "streaming chat definition-erased inner future",
            source: CHAT_SERVICE,
            start: "fn process_message_streaming_with_mode<'a>(",
            end: "async fn process_message_streaming_with_mode_inner(",
            required: &[
                "Pin<Box<dyn std::future::Future<Output = Result<ChatResponse>> + Send + 'a>>",
                "Box::pin(self.process_message_streaming_with_mode_inner(",
            ],
        },
        BoundaryContract {
            label: "Tutor background continuation",
            source: CHAT_SERVICE,
            start: "fn spawn_tutor_background_continuation(",
            end: "pub async fn finalize_copilot_user_preemption(",
            required: &[
                "runtime_boundary::spawn_execution_job(move || async move",
                ".run_tutor_background_continuation(",
            ],
        },
        BoundaryContract {
            label: "public-chat queued replay",
            source: CHAT_SERVICE,
            start: "fn spawn_public_chat_queue_worker_from_arc(",
            end: "fn public_chat_queue_has_items(",
            required: &[
                "runtime_boundary::spawn_execution_job(move || async move",
                ".process_public_chat_queue_loop()",
            ],
        },
        BoundaryContract {
            label: "pending-message replay",
            source: CHAT_SERVICE,
            start: "pub fn spawn_pending_queue_drain(",
            end: "async fn drain_pending_queue(",
            required: &[
                "runtime_boundary::spawn_execution_job(move || async move",
                ".drain_pending_queue(",
            ],
        },
    ];

    for contract in contracts {
        let section = source_section(&contract, contract.source);
        for required in contract.required {
            assert!(
                section.contains(required),
                "{} must retain `{required}`",
                contract.label
            );
        }
    }

    let external_contracts = [
        BoundaryContract {
            label: "non-streaming Chat HTTP API",
            source: CHAT_API,
            start: "pub async fn send_message_handler(",
            end: "pub async fn send_message_stream_handler(",
            required: &["process_message_with_mode_on_execution_runtime_with_app_owner_credential("],
        },
        BoundaryContract {
            label: "streaming Chat HTTP API",
            source: CHAT_API,
            start: "pub async fn send_message_stream_handler(",
            end: "#[cfg(test)]\nmod tests",
            required: &[
                "process_message_with_mode_on_execution_runtime_with_app_owner_credential(",
                "process_message_streaming_with_mode_on_execution_runtime_with_app_owner_credential(",
            ],
        },
        BoundaryContract {
            label: "contextual-writing API",
            source: CONTEXTUAL_WRITING_API,
            start: "pub async fn contextual_writing_action_handler(",
            end: "fn validate_request(",
            required: &["process_message_with_mode_on_execution_runtime("],
        },
        BoundaryContract {
            label: "voice-note API",
            source: MEDIA_API,
            start: "async fn submit_voice_note_chat_turn_on_execution_runtime(",
            end: "pub async fn submit_voice_note_handler(",
            required: &["process_message_with_mode_on_execution_runtime("],
        },
        BoundaryContract {
            label: "voice orchestrator",
            source: VOICE_ORCHESTRATOR,
            start: "pub async fn process_cascaded_voice_turn(",
            end: "pub async fn cancel_cascaded_voice_turn(",
            required: &["process_message_with_mode_on_execution_runtime("],
        },
    ];
    for contract in external_contracts {
        let section = source_section(&contract, contract.source);
        for safe_call in contract.required {
            assert!(
                section.contains(safe_call),
                "{} must retain `{safe_call}`",
                contract.label
            );
        }
        assert!(
            !section.contains(".process_message_with_mode("),
            "{} must not call the current-runtime Chat implementation",
            contract.label
        );
        assert!(
            !section.contains(".process_message_streaming_with_mode("),
            "{} must not call the current-runtime streaming Chat implementation",
            contract.label
        );
    }
}

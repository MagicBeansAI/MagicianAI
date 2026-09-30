import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("eval-llm-observability-phase1.py")
SPEC = importlib.util.spec_from_file_location("eval_llm_observability_phase1", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(MODULE)
OPERATION_ROUTER = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/query_analysis/operation_llm_router.rs"
).read_text(encoding="utf-8")
AGENT_EXECUTOR = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/agentic/executor.rs"
).read_text(encoding="utf-8")
AGENTIC_RUN_STATE = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/agentic/run_loop/state.rs"
).read_text(encoding="utf-8")
AGENTIC_TYPES = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/agentic/types.rs"
).read_text(encoding="utf-8")
V2_ORCHESTRATOR = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/orchestrator/v2_orchestrator.rs"
).read_text(encoding="utf-8")
COMPILED_DISPATCH = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/compiled_dispatch.rs"
).read_text(encoding="utf-8")
COMPILED_PROVIDERS = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/compiled_providers.rs"
).read_text(encoding="utf-8")
AMBIENT_API = (
    Path(__file__).resolve().parents[1]
    / "magician-api/src/ambient_api.rs"
).read_text(encoding="utf-8")
THINKING_MAP_ADAPTER = (
    Path(__file__).resolve().parents[1]
    / "magician-surfaces/src/thinking_map/llm_router_adapter.rs"
).read_text(encoding="utf-8")
CHANNEL_ASSIST_ROOT = (
    Path(__file__).resolve().parents[1] / "magician-comms/src/channel_assist"
)
MAIL_ASSIST_SCOPED_SURFACES = {
    name: (CHANNEL_ASSIST_ROOT / path).read_text(encoding="utf-8")
    for name, path in (
        ("classify", "assist/classify.rs"),
        ("reply_draft", "assist/reply_draft.rs"),
        ("pattern_synthesis", "pattern_synthesis.rs"),
    )
}
MAIL_ASSIST_DISTILL = (CHANNEL_ASSIST_ROOT / "assist/distill.rs").read_text(
    encoding="utf-8"
)
LLM_DISPATCH_SEAM = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/llm_dispatch_seam.rs"
).read_text(encoding="utf-8")
MAGICLLM_ROUTER = (
    Path(__file__).resolve().parents[1] / "magicllm/src/router.rs"
).read_text(encoding="utf-8")
MAGICLLM_BOOTSTRAP = (
    Path(__file__).resolve().parents[1] / "magicllm/src/bootstrap.rs"
).read_text(encoding="utf-8")
MAGICLLM_REALTIME_OPENAI = (
    Path(__file__).resolve().parents[1] / "magicllm/src/realtime/openai.rs"
).read_text(encoding="utf-8")
MAGICLLM_REALTIME_TYPES = (
    Path(__file__).resolve().parents[1] / "magicllm/src/realtime/types.rs"
).read_text(encoding="utf-8")
MAGICLLM_DISPATCH = "\n".join(
    (
        Path(__file__).resolve().parents[1] / "magicllm/src/dispatch" / name
    ).read_text(encoding="utf-8")
    for name in ("worker.rs", "streaming.rs", "tests.rs")
)
MULTI_LLM_SERVICE = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/query_analysis/multi_llm_service.rs"
).read_text(encoding="utf-8")
CHAT_SERVICE = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/chat/service.rs"
).read_text(encoding="utf-8")
CHAT_LLM_SERVICE = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/chat/llm_service.rs"
).read_text(encoding="utf-8")
PRIMITIVE_EXEC_CTX = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/primitive_dispatch/exec_ctx.rs"
).read_text(encoding="utf-8")
PRIMITIVE_COMPILED_PROVIDER = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/primitive_dispatch/compiled_provider.rs"
).read_text(encoding="utf-8")
FLAT_LOOP_DISPATCH = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/flat_loop/dispatch.rs"
).read_text(encoding="utf-8")
RUN_CODING_TASK = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/execution/compiled_handlers/run_coding_task.rs"
).read_text(encoding="utf-8")
VOICE_CONTROL = (
    Path(__file__).resolve().parents[1]
    / "magician-api/src/voice_control_handler.rs"
).read_text(encoding="utf-8")
VOICE_ORCHESTRATOR = (
    Path(__file__).resolve().parents[1]
    / "magician-media/src/media_rails/voice_orchestrator.rs"
).read_text(encoding="utf-8")
MEETING_REALTIME_RESPONDER = (
    Path(__file__).resolve().parents[1]
    / "magician/src/magician_v2/media_seam/meeting_realtime_responder.rs"
).read_text(encoding="utf-8")
REALTIME_VOICE_CLIENT = (
    Path(__file__).resolve().parents[1]
    / "ui/unified-ui/src/lib/media/voice/realtimeVoiceClient.ts"
).read_text(encoding="utf-8")
OPENAI_REALTIME_PROVIDER = (
    Path(__file__).resolve().parents[1]
    / "ui/unified-ui/src/lib/media/voice/providers/openai.ts"
).read_text(encoding="utf-8")


def call(call_id, *, dispatch_job_id=None, attempt=1, parent=None, parent_relation=None,
         retry_group=None, principal="p", workspace="w", reused=False,
         trace_id="trace-1", chat_turn_id=None, iteration_id=None):
    return {
        "schema_version": 1,
        "principal": principal,
        "workspace": workspace,
        "trace_id": trace_id,
        "llm_call_id": call_id,
        "provider_attempt_id": f"{call_id}:a{attempt}",
        "provider_attempt_count": attempt,
        "dispatch_job_id": dispatch_job_id,
        "parent_call_id": parent,
        "parent_relation": parent_relation,
        "retry_group_id": retry_group,
        "scope_resolution": "explicit",
        "workload_class": "foreground_chat" if chat_turn_id else "system",
        "call_role": "primary",
        "chat_turn_id": chat_turn_id,
        "iteration_id": iteration_id,
        "response_reused": reused,
    }


def dispatch(call_id, job_id, *, attempt=1, principal="p", workspace="w", state="completed",
             trace_id="trace-1", parent=None, parent_relation=None, iteration_id=None):
    return {
        "principal": principal,
        "workspace": workspace,
        "trace_id": trace_id,
        "llm_call_id": call_id,
        "provider_attempt_id": f"{call_id}:a{attempt}" if attempt else None,
        "provider_attempt_count": attempt,
        "job_id": job_id,
        "parent_call_id": parent,
        "parent_relation": parent_relation,
        "scope_resolution": "explicit",
        "workload_class": "system",
        "call_role": "primary",
        "iteration_id": iteration_id,
        "response_reused": False,
        "state": state,
    }


def provider_attempt(call_id, attempt=1, *, principal="p", workspace="w",
                     trace_id="trace-1", revision=3):
    return {
        "schema_version": 1,
        "fact_schema_version": 1,
        "record_revision": revision,
        "lifecycle_phase": "completed",
        "principal": principal,
        "workspace": workspace,
        "trace_id": trace_id,
        "llm_call_id": call_id,
        "provider_attempt_id": f"{call_id}:a{attempt}",
        "provider_attempt_index": attempt,
        "scope_resolution": "explicit",
        "workload_class": "system",
        "call_role": "primary",
    }


class Phase1CorrelationAuditTests(unittest.TestCase):
    def test_agentic_chat_lineage_uses_the_task_manifest_and_survives_resume(self):
        self.assertIn(
            "ctx.chat_session_id = task.manifest.chat_session_id;",
            V2_ORCHESTRATOR,
        )
        self.assertIn(
            "pub chat_session_id: Option<String>", AGENTIC_TYPES
        )
        self.assertIn(
            "pause_state.chat_session_id = ctx.chat_session_id.clone();",
            AGENT_EXECUTOR,
        )
        self.assertIn(
            "ctx.chat_session_id = pause_state.chat_session_id.clone();",
            AGENT_EXECUTOR,
        )
        self.assertIn(
            "task_ref = task_ref.with_chat_session(chat_session_id.clone());",
            AGENT_EXECUTOR,
        )
        self.assertIn(
            "correlation.chat_session_id = ctx.chat_session_id.clone();",
            AGENT_EXECUTOR,
        )
        self.assertIn(
            "chat_session_id: ctx.chat_session_id.clone(),",
            AGENT_EXECUTOR,
        )
        self.assertNotIn("chat_session_id_from_execution_id", AGENT_EXECUTOR)
        self.assertNotIn(
            "Future: thread `task.manifest.chat_session_id`", AGENT_EXECUTOR
        )

    def test_chat_session_lineage_reaches_plan_tasks_and_runtime_tool_dispatch(self):
        self.assertIn(
            "chat_session_id: Some(session.id.clone()),",
            CHAT_SERVICE,
        )
        self.assertIn(
            "pub chat_session_id: Option<String>",
            AGENTIC_RUN_STATE,
        )
        self.assertIn(
            "self.chat_session_id = ctx.chat_session_id.clone();",
            AGENTIC_RUN_STATE,
        )
        self.assertIn(
            ".update(|identity| identity.apply_context(ctx));",
            AGENT_EXECUTOR,
        )
        self.assertIn(
            ".with_chat_session(chat_session_id)",
            AGENT_EXECUTOR,
        )
        self.assertIn("pub chat_session_id: Option<String>", PRIMITIVE_EXEC_CTX)
        self.assertIn(
            '"__chat_session_id".to_string()',
            FLAT_LOOP_DISPATCH,
        )
        self.assertIn(
            'provenance_str_opt(&self.provenance, "__chat_session_id")',
            PRIMITIVE_COMPILED_PROVIDER,
        )
        self.assertIn(
            'scope_arg_str(&args, "__chat_session_id")',
            RUN_CODING_TASK,
        )

    def test_inflight_chat_cancellation_keeps_exact_call_identity(self):
        self.assertIn("pub trace_context: Option<magicllm::LlmTraceContext>", CHAT_LLM_SERVICE)
        self.assertIn("pub provider_attempt_counter:", CHAT_LLM_SERVICE)
        self.assertIn("call_transcript_context.trace_context = Some", CHAT_SERVICE)
        self.assertIn(
            "call_transcript_context.provider_attempt_counter =",
            CHAT_SERVICE,
        )
        self.assertIn(
            "let provider_attempt_count = llm_provider_attempt_counter",
            CHAT_SERVICE,
        )
        self.assertIn(
            "LlmTraceReceipt::direct_with_attempt_count(",
            CHAT_SERVICE,
        )
        self.assertIn('error: Some("cancelled".to_string())', CHAT_SERVICE)
        self.assertIn("provider: String::new()", CHAT_SERVICE)
        self.assertIn("model: String::new()", CHAT_SERVICE)
        self.assertIn(
            "fn caller_owned_trace_and_attempt_counter_survive_request_assembly()",
            CHAT_LLM_SERVICE,
        )

    def test_realtime_call_identity_is_minted_before_terminal_usage(self):
        self.assertIn(
            "current_llm_correlation:",
            VOICE_CONTROL,
        )
        begin_call = VOICE_CONTROL.index("fn begin_realtime_llm_call")
        reset_call = VOICE_CONTROL.index("fn reset_realtime_llm_call", begin_call)
        self.assertIn("if self.hands_free", VOICE_CONTROL[begin_call:reset_call])
        self.assertIn(
            "self.begin_realtime_llm_call();",
            VOICE_CONTROL,
        )
        take_failed_call = VOICE_CONTROL.index(
            "fn take_failed_realtime_llm_call", reset_call
        )
        observe_failed_call = VOICE_CONTROL.index(
            "fn observe_failed_realtime_llm_call", take_failed_call
        )
        failed_call_body = VOICE_CONTROL[take_failed_call:observe_failed_call]
        self.assertIn(
            "let correlation = self.current_llm_correlation.take()?;",
            failed_call_body,
        )
        self.assertIn(
            "let started_at_ms = self.current_llm_started_at_ms.take();",
            failed_call_body,
        )
        self.assertIn("self.reset_realtime_llm_call();", VOICE_CONTROL)
        self.assertIn(
            "correlation: Option<magician::magician_v2::realtime_events::LlmEventCorrelation>",
            VOICE_ORCHESTRATOR,
        )
        self.assertIn(
            "usage.is_some() || input_tokens.is_some() || output_tokens.is_some()",
            VOICE_ORCHESTRATOR,
        )
        self.assertIn("take_failed_realtime_llm_call", VOICE_CONTROL)
        self.assertIn("observe_response_failure", VOICE_CONTROL)
        self.assertIn('"response.failed" =>', VOICE_CONTROL)
        self.assertIn(
            "self.observe_failed_realtime_llm_call(error_class, ctx);",
            VOICE_CONTROL,
        )
        self.assertIn("pub async fn observe_response_failure", VOICE_ORCHESTRATOR)
        self.assertIn("success: false", VOICE_ORCHESTRATOR)
        self.assertIn("usage_reported: false", VOICE_ORCHESTRATOR)
        self.assertIn(
            'error: Some(error_class.to_string())',
            VOICE_ORCHESTRATOR,
        )
        self.assertGreaterEqual(
            VOICE_CONTROL.count(
                'self.observe_failed_realtime_llm_call("cancelled", ctx);'
            ),
            5,
            "PTT, explicit interrupt, browser/provider barge-in and session end must close the active call",
        )
        stopped = VOICE_CONTROL.index("fn stopped(&mut self")
        self.assertIn(
            "self.take_failed_realtime_llm_call()",
            VOICE_CONTROL[stopped:],
        )
        start = MEETING_REALTIME_RESPONDER.index("let llm_correlation =")
        dispatch = MEETING_REALTIME_RESPONDER.index("InjectSystemMessage", start)
        terminal = MEETING_REALTIME_RESPONDER.index("correlation: llm_correlation", dispatch)
        self.assertLess(start, dispatch)
        self.assertLess(dispatch, terminal)
        self.assertIn("started_at_ms: llm_started_at_ms", MEETING_REALTIME_RESPONDER)
        self.assertIn(
            "missing_realtime_usage_is_unknown_not_reported_zero",
            MEETING_REALTIME_RESPONDER,
        )
        self.assertNotIn("usage_reported: true", MEETING_REALTIME_RESPONDER)
        self.assertIn("success: response_success", MEETING_REALTIME_RESPONDER)
        self.assertIn(
            'Some("realtime_first_audio_timeout")',
            MEETING_REALTIME_RESPONDER,
        )
        self.assertIn(
            "error: response_error.map(str::to_string)",
            MEETING_REALTIME_RESPONDER,
        )
        self.assertIn(
            "cb.onResponseFailed?.({ responseId, terminalState });",
            OPENAI_REALTIME_PROVIDER,
        )
        self.assertIn("status !== 'completed'", OPENAI_REALTIME_PROVIDER)
        # Both terminal paths must report response.failed. Assert on each path's
        # distinguishing PAYLOAD rather than on which call alias holds the socket:
        # inside these handlers `active` and `call` are the same object (every one
        # of them early-returns on `active !== call`), so pinning `active.ws` vs
        # `call.ws` only pinned an incidental variable name and broke on rename.
        self.assertIn("response_id: responseId", REALTIME_VOICE_CLIENT)
        self.assertIn("terminal_state: terminalState", REALTIME_VOICE_CLIENT)
        self.assertIn(
            "'response.failed', { terminal_state: 'failed' }",
            REALTIME_VOICE_CLIENT,
        )
        self.assertIn("Number.isSafeInteger(value)", OPENAI_REALTIME_PROVIDER)
        self.assertIn("inputText + inputAudio !== inputTotal", OPENAI_REALTIME_PROVIDER)
        self.assertIn("cachedAudio > inputAudio", OPENAI_REALTIME_PROVIDER)
        self.assertIn("usage: usage.and_then(parse_realtime_usage)", MAGICLLM_REALTIME_OPENAI)
        self.assertIn(
            "fn parse_realtime_usage_rejects_missing_or_inconsistent_details()",
            MAGICLLM_REALTIME_OPENAI,
        )
        self.assertIn("ResponseFailed {", MAGICLLM_REALTIME_TYPES)
        self.assertIn("RealtimeResponseTerminalState", MAGICLLM_REALTIME_TYPES)
        self.assertIn("status != Some(\"completed\")", MAGICLLM_REALTIME_OPENAI)
        self.assertIn(
            "fn backend_response_done_preserves_non_success_terminal_state()",
            MAGICLLM_REALTIME_OPENAI,
        )
        self.assertIn(
            "RealtimeProviderEvent::ResponseFailed {\n                response_id,\n                terminal_state,",
            VOICE_CONTROL,
        )
        self.assertIn(
            "RealtimeProviderEvent::ResponseFailed {\n                        response_id: _,\n                        terminal_state,",
            MEETING_REALTIME_RESPONDER,
        )
        response_done = REALTIME_VOICE_CLIENT.index("onResponseDone({")
        token_usage = REALTIME_VOICE_CLIENT.index("sendControl(active.ws, 'token.usage'", response_done)
        response_failed = REALTIME_VOICE_CLIENT.index("onResponseFailed({", token_usage)
        done_block = REALTIME_VOICE_CLIENT[response_done:response_failed]
        self.assertNotIn(
            "inputTokens !== undefined || outputTokens !== undefined",
            done_block,
        )

    def test_effective_route_not_requested_route_owns_attribution(self):
        self.assertIn("pub route_identity: Option<LlmRouteIdentity>", (
            Path(__file__).resolve().parents[1] / "magicllm/src/types.rs"
        ).read_text(encoding="utf-8"))
        self.assertIn("let attempted_route = LlmRouteIdentity", MAGICLLM_ROUTER)
        self.assertIn(
            "response.route_identity = Some(attempted_route.clone())",
            MAGICLLM_ROUTER,
        )
        self.assertIn("terminal_provider_failure_carries_the_effective_route", MAGICLLM_ROUTER)
        self.assertIn(
            "fallback_preflight_failure_retains_the_last_physical_attempt_route",
            MAGICLLM_ROUTER,
        )
        self.assertGreaterEqual(OPERATION_ROUTER.count(".route_identity"), 3)
        self.assertIn("identity.model.as_str()", OPERATION_ROUTER)
        self.assertIn(
            "compute_cost_with_server_web_search_at(\n        &provider_kind,\n        model,",
            OPERATION_ROUTER,
        )
        self.assertIn("queue_failure_metadata_uses_the_terminal_fallback_route", MAGICLLM_DISPATCH)
        self.assertIn("streaming_completion_retains_tokens_and_effective_route_in_job_metadata", MAGICLLM_DISPATCH)
        self.assertIn(
            "outcome_provider_state_follows_the_effective_fallback_route",
            MAGICLLM_DISPATCH,
        )
        self.assertIn(
            "streaming_error_delta_does_not_erase_returned_route_or_error_class",
            MAGICLLM_DISPATCH,
        )
        self.assertIn("route_identity_from_error", MULTI_LLM_SERVICE)
        self.assertIn("route_identity_from_error(&e)", CHAT_SERVICE)
        self.assertIn(
            "receipt.provider_attempt_count == 0",
            CHAT_SERVICE,
        )
        self.assertIn(
            "A configured profile is only a requested-route hint",
            CHAT_SERVICE,
        )
        self.assertIn(
            "chat_telemetry_prefers_effective_fallback_route_for_identity_and_pricing",
            MULTI_LLM_SERVICE,
        )
        self.assertIn("route_identity: Option<&magicllm::LlmRouteIdentity>", MULTI_LLM_SERVICE)
        self.assertIn("response.route_identity.as_ref()", MULTI_LLM_SERVICE)

    def test_profile_specific_provider_clients_are_deterministic(self):
        self.assertIn("profile_providers: HashMap", MAGICLLM_ROUTER)
        self.assertIn("register_profile_provider", MAGICLLM_ROUTER)
        self.assertIn("profile_bound_providers_do_not_cross_endpoints_with_the_same_kind", MAGICLLM_ROUTER)
        self.assertIn("profile_names.sort()", MAGICLLM_BOOTSTRAP)
        self.assertNotIn("arbitrary representative profile", MAGICLLM_BOOTSTRAP)

    def test_non_task_tenant_surfaces_scope_the_router_before_dispatch(self):
        self.assertIn("pub fn with_scope_context", OPERATION_ROUTER)
        self.assertIn("fn authoritative_trace_scope", OPERATION_ROUTER)
        self.assertIn("non_task_scope_is_authoritative_and_task_scope_takes_precedence", OPERATION_ROUTER)
        self.assertIn("operation_router.with_scope_context", AMBIENT_API)
        self.assertIn("router.with_scope_context", THINKING_MAP_ADAPTER)
        for surface, source in MAIL_ASSIST_SCOPED_SURFACES.items():
            with self.subTest(surface=surface):
                self.assertIn("with_scope_context", source)
        self.assertIn("RouterDistillLlm::new(", MAIL_ASSIST_DISTILL)
        self.assertIn("router.with_scope_context", LLM_DISPATCH_SEAM)

    def test_compiled_openai_vision_uses_exact_scoped_router_identity(self):
        self.assertNotIn(
            "https://api.openai.com/v1/chat/completions", COMPILED_PROVIDERS
        )
        self.assertIn("COMPILED_LLM_CALL_CONTEXT", COMPILED_DISPATCH)
        self.assertIn("current_compiled_llm_call_context()", COMPILED_PROVIDERS)
        self.assertIn(
            "generate_for_execution_native_tools_with_trace(", COMPILED_PROVIDERS
        )
        self.assertIn("validate_file_read_path(&path", COMPILED_PROVIDERS)
        self.assertIn("ANALYZE_IMAGE_VIA_OPENAI_MAX_BYTES", COMPILED_PROVIDERS)
        self.assertIn("emit_native_validated_success(", COMPILED_PROVIDERS)

    def test_queue_less_agentic_failures_keep_the_exact_direct_receipt(self):
        self.assertIn("MultiLLMService::traced_route_error", OPERATION_ROUTER)
        self.assertIn("provider_attempt_counter.load(Ordering::Relaxed)", OPERATION_ROUTER)
        self.assertIn("fn emit_direct_route_failure(", OPERATION_ROUTER)
        self.assertIn(
            "direct_operation_failure_bridge_preserves_zero_and_nonzero_attempts",
            OPERATION_ROUTER,
        )
        self.assertIn(
            'response_kind: "provider_error:operation_router_direct".to_string()',
            OPERATION_ROUTER,
        )
        self.assertNotIn("direct_failure_receipt", AGENT_EXECUTOR)

    def test_scope_discovery_rejects_symlinked_workspace(self):
        with tempfile.TemporaryDirectory() as root_value, tempfile.TemporaryDirectory() as external_value:
            runtime_root = Path(root_value)
            workspace = runtime_root / "scopes" / "owner" / "workspace"
            workspace.parent.mkdir(parents=True)
            workspace.symlink_to(Path(external_value), target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "symlink"):
                MODULE.discover_scopes(runtime_root, "owner", "workspace")

    def test_scope_discovery_rejects_path_components(self):
        with tempfile.TemporaryDirectory() as root_value:
            with self.assertRaisesRegex(RuntimeError, "invalid principal"):
                MODULE.discover_scopes(Path(root_value), "..", "workspace")

    def test_parquet_discovery_rejects_symlinked_partition(self):
        with tempfile.TemporaryDirectory() as root_value, tempfile.TemporaryDirectory() as external_value:
            root = Path(root_value)
            external = Path(external_value)
            (external / "part.parquet").write_bytes(b"not-read")
            (root / "dt=2026-07-22").symlink_to(external, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "real directory"):
                MODULE.parquet_files(root)

    def test_dataset_discovery_rejects_symlinked_analytics_ancestor(self):
        with tempfile.TemporaryDirectory() as root_value, tempfile.TemporaryDirectory() as external_value:
            scope_root = Path(root_value) / "scope"
            scope_root.mkdir()
            (scope_root / "analytics").symlink_to(
                Path(external_value), target_is_directory=True
            )
            with self.assertRaisesRegex(RuntimeError, "symlink"):
                MODULE._read_rows(
                    object(),
                    scope_root / "analytics" / "llm_calls",
                    MODULE.CALL_FIELDS,
                    "owner",
                    "workspace",
                    "calls",
                    trusted_scope_root=scope_root,
                )

    def test_missing_gap_count_projects_as_numeric_zero(self):
        projection = MODULE._projection(
            {"gap_reason"}, MODULE.GAP_FIELDS, "owner", "workspace"
        )
        self.assertIn("0::BIGINT AS missing_record_count", projection)

    def test_complete_matrix_passes(self):
        calls = [
            call("direct"),
            call("queued", dispatch_job_id="job-1", attempt=3),
            call("retry-original"),
            call("retry-new", retry_group="retry-original"),
            call("child", parent="direct", parent_relation="verifies"),
            call("chat-first", trace_id="chat-turn", chat_turn_id="turn-1"),
            call("chat-synthesis", trace_id="chat-turn", chat_turn_id="turn-1"),
            call("agent-iteration", dispatch_job_id="job-2", trace_id="execution-root",
                 iteration_id="execution-1:step-1:2"),
            call("queued", dispatch_job_id="job-1", attempt=3, reused=True),
        ]
        rows = [dispatch("queued", "job-1", attempt=3),
                dispatch("agent-iteration", "job-2", trace_id="execution-root",
                         iteration_id="execution-1:step-1:2")]
        audit = MODULE.audit_rows(calls, rows)
        self.assertEqual(audit["status"], "passed")
        self.assertEqual(audit["exact_join_rate"], 1.0)
        self.assertEqual(audit["reused_call_rows"], 1)

    def test_cross_scope_collision_fails(self):
        calls = [call("same", principal="a"), call("same", principal="b", reused=True)]
        audit = MODULE.audit_rows(calls, [])
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(audit["failures"]["cross_scope_call_ids"], ["same"])

    def test_orphan_queue_join_and_bad_attempt_fail(self):
        calls = [call("orphan", dispatch_job_id="missing", attempt=2)]
        calls[0]["provider_attempt_id"] = "wrong"
        audit = MODULE.audit_rows(calls, [])
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(audit["failures"]["ambiguous_or_orphan_queued_calls"], 1)
        self.assertEqual(audit["failures"]["malformed_attempt_ids"], ["wrong"])

    def test_parent_cycle_and_duplicate_producer_fail(self):
        calls = [call("a", parent="b", parent_relation="supports"),
                 call("b", parent="a", parent_relation="supports"), call("a")]
        audit = MODULE.audit_rows(calls, [])
        self.assertEqual(audit["status"], "failed")
        self.assertTrue(audit["failures"]["parent_cycle"])
        self.assertEqual(audit["failures"]["duplicate_producer_call_ids"], ["a"])

    def test_retry_group_cannot_reference_the_same_logical_call(self):
        audit = MODULE.audit_rows([call("self-retry", retry_group="self-retry")], [])

        self.assertEqual(audit["status"], "failed")
        self.assertEqual(
            audit["failures"]["self_referencing_retry_groups"],
            ["p/w:self-retry"],
        )

    def test_historical_rows_are_reported_as_no_phase1_data(self):
        audit = MODULE.audit_rows([{"schema_version": 0, "llm_call_id": None}], [])
        self.assertEqual(audit["status"], "no_phase1_data")

    def test_cancelled_before_provider_does_not_require_attempt_id(self):
        cancelled = call("cancelled", dispatch_job_id="job", attempt=0)
        cancelled["provider_attempt_id"] = None
        audit = MODULE.audit_rows(
            [cancelled],
            [dispatch("cancelled", "job", attempt=0, state="tombstoned")],
        )
        self.assertEqual(audit["status"], "passed")
        self.assertEqual(audit["failures"]["missing_attempt_ids"], 0)

    def test_terminal_dispatch_without_call_fact_fails(self):
        audit = MODULE.audit_rows(
            [], [dispatch("failed", "job", attempt=1, state="failed")]
        )
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(
            audit["failures"]["terminal_dispatch_without_call"], ["p/w:job"]
        )

    def test_canonical_revisions_and_legacy_mirror_are_one_call(self):
        started = call("canonical", dispatch_job_id="job")
        started.update({"fact_schema_version": 2, "record_revision": 1, "lifecycle_phase": "started"})
        started["provider_attempt_id"] = None
        started["provider_attempt_count"] = 0
        completed = call("canonical", dispatch_job_id="job")
        completed.update({"fact_schema_version": 2, "record_revision": 2, "lifecycle_phase": "completed"})
        legacy = call("canonical", dispatch_job_id="job")
        audit = MODULE.audit_rows(
            [started, completed, legacy], [dispatch("canonical", "job")]
        )
        self.assertEqual(audit["status"], "passed")
        self.assertEqual(audit["phase1_call_rows"], 1)

    def test_queue_join_rejects_trace_or_attempt_mismatch(self):
        calls = [call("queued", dispatch_job_id="job", attempt=2)]
        rows = [dispatch("queued", "job", attempt=1, trace_id="other-trace")]
        audit = MODULE.audit_rows(calls, rows)
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(
            audit["failures"]["mismatched_queue_correlations"],
            ["queued:provider_attempt_count", "queued:provider_attempt_id", "queued:trace_id"],
        )

    def test_canonical_call_uses_normalized_provider_attempt_identity(self):
        completed = call("canonical-direct", attempt=2)
        completed.update({
            "fact_schema_version": 1,
            "record_revision": 2,
            "lifecycle_phase": "completed",
            "provider_attempt_id": None,
        })
        audit = MODULE.audit_rows(
            [completed],
            [],
            [provider_attempt("canonical-direct", 2)],
        )
        self.assertEqual(audit["status"], "passed")
        self.assertEqual(audit["phase1_attempt_rows"], 1)

    def test_canonical_direct_call_without_attempt_evidence_fails(self):
        completed = call("canonical-direct", attempt=2)
        completed.update({
            "fact_schema_version": 1,
            "record_revision": 2,
            "lifecycle_phase": "completed",
            "provider_attempt_id": None,
        })
        audit = MODULE.audit_rows([completed], [], [])
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(audit["failures"]["missing_attempt_ids"], 1)

    def test_terminal_attempt_gap_accounts_for_unavailable_direct_failure(self):
        completed = call("failed-direct", attempt=2)
        completed.update({
            "fact_schema_version": 2,
            "record_revision": 2,
            "lifecycle_phase": "completed",
            "provider_attempt_id": None,
        })
        gaps = [{
            "principal": "p",
            "workspace": "w",
            "llm_call_id": "failed-direct",
            "gap_reason": "terminal_provider_attempt_lifecycle_unavailable",
            "missing_record_count": 2,
        }]
        audit = MODULE.audit_rows([completed], [], [], gaps)
        self.assertEqual(audit["status"], "passed")
        # One final attempt identity is absent from the normalized attempt
        # table; the gap count can be larger because it accounts for the full
        # unavailable lifecycle across both physical attempts.
        self.assertEqual(audit["documented_unavailable_attempt_ids"], 1)
        self.assertEqual(audit["failures"]["missing_attempt_ids"], 0)

    def test_terminal_attempt_gap_cannot_mask_another_calls_missing_identity(self):
        first = call("first", attempt=1)
        first.update({
            "fact_schema_version": 2,
            "record_revision": 2,
            "lifecycle_phase": "completed",
            "provider_attempt_id": None,
        })
        second = call("second", attempt=1)
        second.update({
            "fact_schema_version": 2,
            "record_revision": 2,
            "lifecycle_phase": "completed",
            "provider_attempt_id": None,
        })
        gaps = [{
            "principal": "p",
            "workspace": "w",
            "llm_call_id": "second",
            "gap_reason": "terminal_provider_attempt_lifecycle_unavailable",
            "missing_record_count": 1,
        }]

        audit = MODULE.audit_rows([first, second], [], [], gaps)

        self.assertEqual(audit["status"], "failed")
        self.assertEqual(audit["documented_unavailable_attempt_ids"], 1)
        self.assertEqual(audit["failures"]["missing_attempt_ids"], 1)

    def test_duplicate_canonical_revision_keys_fail_before_coalescing(self):
        completed = call("canonical-duplicate", attempt=1)
        completed.update({
            "fact_schema_version": 2,
            "record_revision": 2,
            "lifecycle_phase": "completed",
            "provider_attempt_id": None,
        })
        attempt = provider_attempt("canonical-duplicate", 1)

        audit = MODULE.audit_rows(
            [completed, dict(completed)],
            [],
            [attempt, dict(attempt)],
        )

        self.assertEqual(audit["status"], "failed")
        self.assertEqual(
            audit["failures"]["duplicate_canonical_call_revisions"],
            ["p/w:canonical-duplicate:r2"],
        )
        self.assertEqual(
            audit["failures"]["duplicate_canonical_attempt_revisions"],
            ["p/w:canonical-duplicate:a1:r3"],
        )

    def test_malformed_canonical_attempt_ordinal_fails(self):
        completed = call("canonical-direct", attempt=2)
        completed.update({
            "fact_schema_version": 1,
            "record_revision": 2,
            "lifecycle_phase": "completed",
            "provider_attempt_id": None,
        })
        bad = provider_attempt("canonical-direct", 2)
        bad["provider_attempt_index"] = 1
        audit = MODULE.audit_rows([completed], [], [bad])
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(
            audit["failures"]["malformed_attempt_ids"],
            ["canonical-direct:a2"],
        )

    def test_duplicate_attempt_failure_preserves_full_attempt_identity(self):
        duplicate = call("duplicate")
        audit = MODULE.audit_rows(
            [duplicate, dict(duplicate)],
            [dispatch("duplicate", "job"), dispatch("duplicate", "job")],
        )
        self.assertEqual(
            audit["failures"]["duplicate_attempt_ids"], ["duplicate:a1"]
        )

    def test_parent_relation_and_scope_are_mandatory(self):
        bad_parent = call("child", parent="missing")
        bad_parent["principal"] = ""
        bad_parent["scope_resolution"] = "guessed"
        audit = MODULE.audit_rows([bad_parent], [])
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(audit["failures"]["missing_parent_relations"], ["child"])
        self.assertEqual(audit["failures"]["orphan_parent_ids"], ["/w:missing"])
        self.assertEqual(audit["failures"]["invalid_scopes"], ["child"])
        self.assertEqual(audit["failures"]["invalid_scope_resolutions"], ["child"])

    def test_identity_whitespace_and_embedded_scope_drift_fail_closed(self):
        malformed = call(" call-with-space")
        malformed["_source_principal"] = "owner"
        malformed["_source_workspace"] = "w"
        audit = MODULE.audit_rows([malformed], [])
        self.assertEqual(audit["status"], "failed")
        self.assertEqual(audit["failures"]["embedded_scope_mismatches"], 1)
        self.assertIn(
            "calls:llm_call_id",
            audit["failures"]["noncanonical_identity_fields"],
        )

    def test_attempt_context_and_parent_trace_drift_fail_closed(self):
        parent = call("parent", trace_id="trace-parent")
        child = call(
            "child",
            trace_id="trace-child",
            parent="parent",
            parent_relation="supports",
        )
        attempt = provider_attempt("child", trace_id="trace-attempt")
        audit = MODULE.audit_rows([parent, child], [], [attempt])
        self.assertEqual(audit["status"], "failed")
        self.assertIn("trace_id", audit["failures"]["context_drift_fields"])
        self.assertEqual(
            audit["failures"]["parent_identity_mismatches"], ["child"]
        )

    def test_parent_and_retry_references_cannot_resolve_through_another_scope(self):
        foreign_parent = call("parent", principal="other")
        child = call(
            "child",
            parent="parent",
            parent_relation="supports",
            principal="owner",
        )
        retry = call("retry", retry_group="parent", principal="owner")

        audit = MODULE.audit_rows([foreign_parent, child, retry], [])

        self.assertEqual(audit["status"], "failed")
        self.assertEqual(
            audit["failures"]["orphan_parent_ids"], ["owner/w:parent"]
        )
        self.assertEqual(
            audit["failures"]["retry_groups_without_member"], ["owner/w:parent"]
        )

    def test_logical_coordinator_without_physical_attempt_is_valid(self):
        coordinator = call("logical", attempt=0)
        coordinator["provider_attempt_id"] = None
        child = dispatch(
            "chunk", "job", parent="logical", parent_relation="chunk_map"
        )
        child_call = call(
            "chunk", dispatch_job_id="job", parent="logical", parent_relation="chunk_map"
        )
        audit = MODULE.audit_rows([coordinator, child_call], [child])
        self.assertEqual(audit["status"], "passed")


if __name__ == "__main__":
    unittest.main()

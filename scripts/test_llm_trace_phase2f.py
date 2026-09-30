#!/usr/bin/env python3
"""Provider-free structural regressions for Phase 2F activation."""

from __future__ import annotations

import json
import importlib.util
import pathlib
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
CONTRACT = json.loads(
    (
        ROOT
        / "data"
        / "magician_v2"
        / "llm_observability"
        / "phase2-activation-contract-v1.json"
    ).read_text(encoding="utf-8")
)
ANALYTICS = ROOT / "magician/src/magician_v2/analytics"
ACTIVATION = (ANALYTICS / "llm_trace_activation.rs").read_text(encoding="utf-8")
JOURNAL = (ANALYTICS / "llm_trace_journal.rs").read_text(encoding="utf-8")
MATERIALIZER = (ANALYTICS / "llm_trace_materializer.rs").read_text(encoding="utf-8")
READER = (ANALYTICS / "llm_analytics_read_service.rs").read_text(encoding="utf-8")
RECORDER = (ANALYTICS / "llm_trace_recorder.rs").read_text(encoding="utf-8")
RETENTION = (ANALYTICS / "llm_parquet_sink.rs").read_text(encoding="utf-8")
MAINTENANCE = (ANALYTICS / "parquet_maintenance.rs").read_text(encoding="utf-8")
REPRICE = (ANALYTICS / "llm_reprice.rs").read_text(encoding="utf-8")
PRICING_IDENTITY = (ANALYTICS / "llm_pricing_identity.rs").read_text(encoding="utf-8")
DISPATCH_ROWS = (ANALYTICS / "llm_dispatch_rows.rs").read_text(encoding="utf-8")
RUNTIME = (ROOT / "magician-bin/src/main.rs").read_text(encoding="utf-8")
NATIVE_TYPES = (
    ROOT / "magician/src/magician_v2/execution/agentic/native_types.rs"
).read_text(encoding="utf-8")
NATIVE_ADAPTER = (
    ROOT / "magician/src/magician_v2/execution/agentic/native_adapter.rs"
).read_text(encoding="utf-8")
NATIVE_INTEGRATION = (
    ROOT / "magician/src/magician_v2/execution/agentic/native_integration.rs"
).read_text(encoding="utf-8")
DECISION_ADAPTER = (
    ROOT / "magician/src/magician_v2/execution/multi_llm_agent_adapter.rs"
).read_text(encoding="utf-8")
AGENT_EXECUTOR = (
    ROOT / "magician/src/magician_v2/execution/agentic/executor.rs"
).read_text(encoding="utf-8")
AGENT_DECISION = (
    ROOT / "magician/src/magician_v2/execution/agentic/decision.rs"
).read_text(encoding="utf-8")
AGENT_DECIDE_PHASE = (
    ROOT / "magician/src/magician_v2/execution/agentic/run_loop/phases/decide.rs"
).read_text(encoding="utf-8")
OPERATION_ROUTER = (
    ROOT / "magician/src/magician_v2/query_analysis/operation_llm_router.rs"
).read_text(encoding="utf-8")
OPERATION_TELEMETRY = (
    ANALYTICS / "operation_llm_telemetry.rs"
).read_text(encoding="utf-8")
MULTI_LLM_SERVICE = (
    ROOT / "magician/src/magician_v2/query_analysis/multi_llm_service.rs"
).read_text(encoding="utf-8")
CHAT_SERVICE = (
    ROOT / "magician/src/magician_v2/chat/service.rs"
).read_text(encoding="utf-8")
SCREEN_OBSERVE = (
    ROOT / "magician-media/src/media_rails/screen_observe.rs"
).read_text(encoding="utf-8")
EVIDENCE_CALLERS = "\n".join(
    (ROOT / "magician/src/magician_v2/evidence" / name).read_text(encoding="utf-8")
    for name in ("mod.rs", "screen_distill.rs", "tier_distill.rs")
)
ARTIFACT_CALLERS = "\n".join(
    (ROOT / "magician/src/magician_v2/artifact_v2" / name).read_text(encoding="utf-8")
    for name in ("service.rs", "synthesis.rs")
)
ORCHESTRATOR = (
    ROOT / "magician/src/magician_v2/orchestrator/v2_orchestrator.rs"
).read_text(encoding="utf-8")
MAGICLLM_ROUTER = (ROOT / "magicllm/src/router.rs").read_text(encoding="utf-8")
MAGICLLM_DISPATCH = "\n".join(
    (ROOT / "magicllm/src/dispatch" / name).read_text(encoding="utf-8")
    for name in ("worker.rs", "streaming.rs", "tests.rs")
)
COMPILED_PROVIDERS = (
    ROOT / "magician/src/magician_v2/execution/compiled_providers.rs"
).read_text(encoding="utf-8")
API = (ROOT / "magician-api/src/analytics_api.rs").read_text(
    encoding="utf-8"
)
UI = (ROOT / "ui/unified-ui/src/routes/(app)/llm/+page.svelte").read_text(
    encoding="utf-8"
)
EVALUATOR = (ROOT / "scripts/eval-llm-observability-phase2f.py").read_text(
    encoding="utf-8"
)
LIVE_RUNNER = (ROOT / "scripts/run-live-evals-with-report.sh").read_text(
    encoding="utf-8"
)
MAKEFILE = (ROOT / "Makefile").read_text(encoding="utf-8")
EVALUATOR_PATH = ROOT / "scripts/eval-llm-observability-phase2f.py"
EVALUATOR_SPEC = importlib.util.spec_from_file_location(
    "eval_llm_observability_phase2f", EVALUATOR_PATH
)
EVALUATOR_MODULE = importlib.util.module_from_spec(EVALUATOR_SPEC)
assert EVALUATOR_SPEC and EVALUATOR_SPEC.loader
EVALUATOR_SPEC.loader.exec_module(EVALUATOR_MODULE)


class Phase2ActivationContractTests(unittest.TestCase):
    def test_live_reader_rejects_scope_and_partition_symlink_redirection(self) -> None:
        with tempfile.TemporaryDirectory() as root_value, tempfile.TemporaryDirectory() as external_value:
            runtime_root = pathlib.Path(root_value)
            external = pathlib.Path(external_value)
            workspace = runtime_root / "scopes" / "owner" / "workspace"
            workspace.parent.mkdir(parents=True)
            workspace.symlink_to(external, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "symlink"):
                EVALUATOR_MODULE.ensure_no_symlink_components(
                    runtime_root, workspace / "analytics"
                )
            with self.assertRaisesRegex(RuntimeError, "invalid principal"):
                EVALUATOR_MODULE.validate_scope_component("..", "principal")

            dataset = runtime_root / "safe-dataset"
            dataset.mkdir()
            partition = dataset / "dt=2026-07-22"
            partition.symlink_to(external, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "real directory"):
                EVALUATOR_MODULE.safe_parquet_files(
                    dataset, "dt=*/*.parquet"
                )

    def test_activation_is_default_single_owned_and_gracefully_drained(self) -> None:
        self.assertEqual(
            CONTRACT["activation"], "active_by_default_without_feature_switch"
        )
        self.assertRegex(RUNTIME, r"let\s+mut\s+llm_trace_activation\s*=")
        self.assertIn("LlmTraceActivation::start(", RUNTIME)
        self.assertIn("llm_trace_activation.shutdown().await", RUNTIME)
        self.assertIn("storage_maintenance.shutdown().await", RUNTIME)
        self.assertIn("cancel_for_task.cancelled()", MAINTENANCE)
        self.assertIn("pub async fn shutdown(mut self)", MAINTENANCE)
        self.assertNotIn("LlmParquetRetention::spawn", RUNTIME)
        self.assertNotIn("MAGICIAN_LLM_TRACE", RUNTIME + ACTIVATION)
        canonical_start = RUNTIME.index("LlmTraceActivation::start(")
        mirror_start = RUNTIME.index("LlmParquetSink::spawn(")
        supervisor_start = RUNTIME.index("spawn_agent_supervisor_tasks(", canonical_start)
        self.assertLess(mirror_start, canonical_start)
        self.assertLess(canonical_start, supervisor_start)
        shutdown = RUNTIME.index("llm_trace_activation.shutdown().await")
        producers_cancel = RUNTIME.index("supervisor_shutdown.cancel()")
        producers_join = RUNTIME.index(
            "for (task_name, handle) in supervisor_tasks.iter_mut()", producers_cancel
        )
        maintenance_shutdown = RUNTIME.index(
            "storage_maintenance.shutdown().await", producers_join
        )
        analytics_cancel = RUNTIME.index("analytics_shutdown.cancel()", shutdown)
        mirror_shutdown = RUNTIME.index("_llm_parquet_sink.shutdown().await", shutdown)
        self.assertLess(producers_cancel, producers_join)
        self.assertLess(producers_join, maintenance_shutdown)
        self.assertLess(maintenance_shutdown, shutdown)
        self.assertLess(shutdown, mirror_shutdown)
        self.assertLess(mirror_shutdown, analytics_cancel)
        self.assertLess(shutdown, analytics_cancel)

        activation_start = ACTIVATION.index("pub fn start(")
        subscribe = ACTIVATION.index("broadcaster.subscribe()", activation_start)
        recovery = ACTIVATION.index("discover_recovery_scopes", subscribe)
        pipeline = ACTIVATION.index("build_canonical_llm_trace_pipeline", recovery)
        self.assertLess(subscribe, recovery)
        self.assertLess(recovery, pipeline)
        self.assertEqual(
            CONTRACT["recovery"]["startup"],
            "canonical_and_compatibility_receivers_subscribe_before_recovery_then_replay_uncommitted_before_new_scope_commit",
        )

    def test_bridge_maps_stable_scoped_lifecycle_without_inventing_retries(self) -> None:
        for record in (
            "LlmTraceRecord::CallStarted(started)",
            "LlmTraceRecord::ProviderAttempt(attempt)",
            "LlmTraceRecord::CallCompleted(completed)",
        ):
            self.assertIn(record, ACTIVATION)
        self.assertIn("missing authoritative scope", ACTIVATION)
        self.assertIn("missing stable call correlation", ACTIVATION)
        self.assertIn(
            "successful response requires one or more provider attempts", ACTIVATION
        )
        self.assertIn("missing provider attempt id", ACTIVATION)
        self.assertIn("missing_or_invalid_start_timestamp", ACTIVATION)
        self.assertNotIn("derived_start", ACTIVATION)
        self.assertIn("EARLIER_ATTEMPTS_UNAVAILABLE", ACTIVATION)
        self.assertIn("u64::from(provider_attempt_index - 1)", ACTIVATION)
        self.assertIn("if correlation.response_reused", ACTIVATION)
        self.assertIn(
            "fn reused_response_does_not_materialize_a_second_lifecycle()",
            ACTIVATION,
        )
        self.assertEqual(
            CONTRACT["non_invention"]["categorical_content_boundary"],
            "response_validation_and_gap_categories_are_bounded_machine_tokens_and_arbitrary_diagnostic_prose_collapses_to_a_fixed_reason_code",
        )
        self.assertEqual(
            CONTRACT["migration"]["compatibility_content_boundary"],
            "new_rows_omit_reasoning_summary_replace_free_form_error_with_a_fixed_presence_marker_and_redact_malformed_response_categories",
        )
        self.assertIn(
            "fn phase1_call_row_maps_identity_audio_and_redacts_restricted_content()",
            RETENTION,
        )
        self.assertIn("legacy_error_redacted", RETENTION)
        self.assertIn("invalid_category_redacted", RETENTION)
        self.assertIn("is_content_free_machine_category(response_kind)", ACTIVATION)
        self.assertIn("fn mapping_reason_code(value: &str) -> &'static str", ACTIVATION)
        self.assertNotIn("fn reason_slug(", ACTIVATION)
        self.assertIn('"mapping_validation_failed"', ACTIVATION)
        self.assertIn('"record_oversize"', ACTIVATION)
        self.assertIn("categorical_content_violation_count", EVALUATOR)
        self.assertIn("def is_machine_category", EVALUATOR)
        self.assertIn(
            '"response_kind": "private user text"', EVALUATOR
        )
        self.assertIn(
            "fn response_categories_reject_content_and_mapping_gap_reasons_are_fixed()",
            ACTIVATION,
        )
        self.assertIn(
            "fn validation_class_normalization_never_persists_prose_or_oversized_input()",
            OPERATION_TELEMETRY,
        )
        self.assertIn(
            "fn opaque_external_run_does_not_fabricate_a_provider_attempt()",
            ACTIVATION,
        )
        self.assertEqual(
            CONTRACT["non_invention"]["opaque_external_ai_run"],
            "compatibility_aggregate_ignored_by_canonical_call_and_attempt_capture",
        )
        self.assertIn(
            "fn missing_start_timestamp_is_rejected_instead_of_derived_from_latency()",
            ACTIVATION,
        )
        self.assertIn("INVALID_TTFT", ACTIVATION)
        self.assertIn(
            "runtime_transport_events_unclassified_due_broadcast_lag", ACTIVATION
        )
        self.assertIn("fn submit_mapped_record(", ACTIVATION)
        self.assertIn(
            "fn dispatch_lag_is_counted_as_both_transport_loss_and_an_emitted_gap()",
            ACTIVATION,
        )
        self.assertEqual(
            CONTRACT["non_invention"]["activation_gap_metrics"],
            "every_mapper_attempt_and_transport_or_dispatch_lag_gap_submission_is_counted_once",
        )
        self.assertIn("pricing_version_at(&provider_kind, model, started_at_ms)", ACTIVATION)
        self.assertIn("realtime_pricing_version_at(model, started_at_ms)", ACTIVATION)
        self.assertIn("PRICING_ROW_VERSION_PREFIX", EVALUATOR)
        self.assertEqual(
            CONTRACT["non_invention"]["pricing_version"],
            "content_free_blake3_fingerprint_of_the_exact_effective_rate_row_including_rates_date_prefix_and_long_context_rule",
        )
        self.assertIn("runtime-pricing-miss@call-time", EVALUATOR)
        self.assertIn("runtime-pricing-invalid-value@call-time", EVALUATOR)
        self.assertIn("provider-usage-unreported@call-time", ACTIVATION)
        self.assertIn("RUNTIME_MIRROR_EXPECTED_PRICING_VERSIONS", EVALUATOR)
        self.assertEqual(
            CONTRACT["non_invention"]["producer_cost_mismatch"],
            "effective_dated_canonical_recompute_wins_and_explicit_diagnostic_gap_is_emitted",
        )
        self.assertEqual(
            CONTRACT["non_invention"]["pricing_provider_identity"],
            "one_shared_finite_explicit_adapter_alias_map_for_live_capture_and_historical_repricing_and_realtime_rates_require_openai_provider_plus_model",
        )
        self.assertIn("producer_cost_mismatch_recomputed", ACTIVATION)
        self.assertIn("provider_kind_for_pricing(provider)", ACTIVATION)
        self.assertIn("provider_kind_for_pricing(&provider)", REPRICE)
        self.assertIn('"openai_realtime_backend"', PRICING_IDENTITY)
        self.assertIn(
            "fn vendor_looking_custom_providers_never_inherit_vendor_pricing()",
            PRICING_IDENTITY,
        )
        self.assertIn("magicllm::compute_cost_at", ACTIVATION)
        self.assertIn("magicllm::compute_realtime_cost_at", ACTIVATION)
        self.assertIn(
            "fn producer_cost_mismatch_is_recomputed_and_visible_as_a_gap()",
            ACTIVATION,
        )
        self.assertIn(
            "fn inconsistent_usage_is_omitted_instead_of_saturating_into_a_price()",
            ACTIVATION,
        )
        self.assertIn(
            "fn custom_provider_prefix_never_inherits_vendor_pricing()",
            ACTIVATION,
        )
        self.assertIn(
            "fn custom_provider_cannot_inherit_openai_realtime_pricing_by_model_name()",
            ACTIVATION,
        )
        self.assertNotIn('normalized.starts_with("openai")', ACTIVATION)
        self.assertIn(
            "fn realtime_pricing_reconstructs_uncached_and_cached_modalities_once()",
            ACTIVATION,
        )
        self.assertIn(
            "fn realtime_reprice_does_not_double_bill_cached_input_as_uncached_text()",
            REPRICE,
        )
        self.assertIn(
            "fn historical_runtime_adapter_alias_uses_the_same_pricing_identity_as_live_capture()",
            REPRICE,
        )
        self.assertIn(
            "fn invalid_or_unpriced_rows_are_never_rewritten_as_plausible_zero_cost()",
            REPRICE,
        )
        self.assertNotIn("fn clamp_token(", REPRICE)
        self.assertIn("legacy_only_count", EVALUATOR)
        self.assertEqual(
            CONTRACT["non_invention"]["missing_provider_usage"],
            "null_usage_and_cost_with_explicit_gap_never_fabricated_zero",
        )
        self.assertEqual(
            CONTRACT["non_invention"]["logical_chunk_parent"],
            "zero_attempt_non_billing_summary_with_physical_children_captured_individually",
        )
        self.assertEqual(
            CONTRACT["non_invention"]["provider_attempt_timing"],
            "exact_for_direct_single_attempt_only_otherwise_terminal_completion_without_invented_start_or_latency",
        )
        self.assertEqual(
            CONTRACT["reconciliation"]["compatibility_rows_without_canonical_lifecycle"],
            "zero_except_explicit_external_ai_run_exclusion",
        )
        self.assertIn("safe_parquet_files", EVALUATOR)
        self.assertIn("cost_source: Some(LlmCostSource::Computed)", ACTIVATION)
        self.assertIn("TERMINAL_ATTEMPTS_UNAVAILABLE", ACTIVATION)
        self.assertEqual(
            CONTRACT["non_invention"]["failed_provider_attempts"],
            "materialize_the_known_terminal_attempt_and_gap_only_earlier_or_incomplete_attempts",
        )
        self.assertEqual(
            CONTRACT["non_invention"]["successful_response_missing_effective_route"],
            "logical_terminal_fact_and_usage_preserved_with_call_owned_unavailable_attempt_gap_and_unknown_pricing",
        )
        self.assertIn(
            "fn successful_response_without_effective_route_keeps_call_and_gaps_attempt()",
            ACTIVATION,
        )
        self.assertIn(
            "fn attempted_chat_response_never_uses_requested_route_as_effective_route()",
            MULTI_LLM_SERVICE,
        )
        self.assertIn(
            "fn pre_provider_failure_materializes_call_without_attempt_or_gap()",
            ACTIVATION,
        )
        self.assertIn(
            "fn logical_chunk_parent_is_a_non_billing_zero_attempt_summary()",
            ACTIVATION,
        )
        self.assertIn(
            "successful response requires one or more provider attempts",
            ACTIVATION,
        )
        self.assertIn("!logical_chunk_summary && !harness_aggregate", ACTIVATION)
        self.assertIn("LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND", RECORDER)
        self.assertIn("response_kind = 'logical_chunk_summary'", READER)
        self.assertIn(
            "fn execution_native_contract_failure_preserves_successful_attempt_and_usage()",
            ACTIVATION,
        )
        self.assertIn("fn validate_mapped_records(", ACTIVATION)
        self.assertIn(
            "fn mapped_record_validation_fails_as_one_gap_without_partial_lifecycle()",
            ACTIVATION,
        )
        self.assertIn(
            "fn missing_provider_usage_stays_null_and_cannot_be_priced_as_zero()",
            ACTIVATION,
        )
        self.assertIn("emit_failed_physical_attempt", OPERATION_ROUTER)
        self.assertIn(
            'response_kind: "provider_error:logical_chunk_physical".to_string()',
            OPERATION_ROUTER,
        )
        self.assertIn(
            "fn direct_logical_chunk_failure_preserves_the_physical_child_attempt()",
            OPERATION_ROUTER,
        )
        self.assertIn(
            "fn failed_response_materializes_only_known_terminal_attempt_and_gaps_earlier_ones()",
            ACTIVATION,
        )
        self.assertIn(
            "fn failed_dispatch_materializes_effective_terminal_attempt()",
            ACTIVATION,
        )
        self.assertIn("fn terminal_attempt_timing(", ACTIVATION)
        self.assertIn(
            "Dispatch metadata aggregates provider execution across the whole job",
            ACTIVATION,
        )
        self.assertIn("d.provider_attempt_count = 1", READER)
        self.assertIn("LEFT JOIN llm_dispatch_timing d", READER)

    def test_execution_native_projection_preserves_receipt_and_contract_failures(self) -> None:
        self.assertIn("pub telemetry: Option<", NATIVE_TYPES)
        self.assertEqual(NATIVE_ADAPTER.count("response.telemetry = raw.telemetry"), 2)
        self.assertIn(
            "if let Some(telemetry) = response.telemetry.clone()", NATIVE_INTEGRATION
        )
        self.assertIn("NativeDecisionResponseError", DECISION_ADAPTER)
        self.assertIn("validation_failure_from_error", DECISION_ADAPTER)
        self.assertIn(
            "MultiLlmAgentAdapter::validation_failure_from_error(error)",
            AGENT_DECISION,
        )
        self.assertIn(
            "validation_error:{validation_error_class}", AGENT_DECIDE_PHASE
        )
        self.assertIn("if let Some(correlation) = correlation", AGENT_DECIDE_PHASE)
        self.assertIn("contract_validation_success: Some(false)", ACTIVATION)

    def test_compiled_vision_success_and_direct_failure_share_canonical_bridge(self) -> None:
        self.assertIn("fn emit_direct_route_failure(", OPERATION_ROUTER)
        self.assertIn(
            "direct_operation_failure_bridge_preserves_zero_and_nonzero_attempts",
            OPERATION_ROUTER,
        )
        self.assertIn("emit_native_validated_success(", COMPILED_PROVIDERS)
        self.assertIn(
            'response_kind: "provider_error:operation_router_direct".to_string()',
            OPERATION_ROUTER,
        )

    def test_fallback_and_streaming_attribution_use_the_effective_physical_route(self) -> None:
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
        self.assertIn("terminal_tokens = response.usage.as_ref()", MAGICLLM_DISPATCH)
        self.assertIn("terminal_route_identity = response.route_identity.clone()", MAGICLLM_DISPATCH)
        self.assertIn("queue_failure_metadata_uses_the_terminal_fallback_route", MAGICLLM_DISPATCH)
        self.assertIn(
            "outcome_provider_state_follows_the_effective_fallback_route",
            MAGICLLM_DISPATCH,
        )
        self.assertIn(
            "streaming_error_delta_does_not_erase_returned_route_or_error_class",
            MAGICLLM_DISPATCH,
        )
        self.assertIn("route_identity_from_error", MULTI_LLM_SERVICE)
        self.assertIn(
            "traced_route_failure_exposes_the_effective_fallback_route",
            MULTI_LLM_SERVICE,
        )
        self.assertIn("route_identity_from_error(&e)", CHAT_SERVICE)
        self.assertIn(
            "chat_telemetry_prefers_effective_fallback_route_for_identity_and_pricing",
            MULTI_LLM_SERVICE,
        )
        self.assertIn(
            "dispatch_parquet_round_trip_preserves_effective_profile_and_scope",
            DISPATCH_ROWS,
        )

    def test_immediate_validation_is_authoritative_content_free_and_not_inferred(self) -> None:
        self.assertEqual(
            CONTRACT["non_invention"]["transport_only_response"],
            "validation_attempted_remains_false_until_an_authoritative_caller_parser_reports_an_outcome",
        )
        self.assertIn("emit_post_response_validation_failure", OPERATION_ROUTER)
        self.assertIn("validation_error:{validation_class}", OPERATION_TELEMETRY)
        self.assertIn(
            "caller contract validation failed ({validation_class})",
            OPERATION_TELEMETRY,
        )
        self.assertIn("let _ = error;", OPERATION_TELEMETRY)
        self.assertIn(
            "explicit_validation_success_is_not_inferred_from_transport_alone",
            ACTIVATION,
        )
        self.assertIn("narration_verdict_accepts_only_the_declared_schema", SCREEN_OBSERVE)
        self.assertIn("emit_validated_success", EVIDENCE_CALLERS)
        self.assertIn("emit_validation_failure", EVIDENCE_CALLERS)
        self.assertIn("validate_synthesized_output_envelope", ARTIFACT_CALLERS)
        self.assertIn("emit_validated_success", ARTIFACT_CALLERS)
        self.assertIn(
            "parse_and_validate_decomposed_stages_with_status",
            ORCHESTRATOR,
        )
        self.assertIn(
            "unauthorized stage must be visible to telemetry",
            ORCHESTRATOR,
        )

    def test_legacy_writers_are_compatibility_only_and_stable_reads_deduplicate(self) -> None:
        self.assertEqual(
            CONTRACT["migration"]["llm_calls"], "compatibility_mirror_only"
        )
        self.assertEqual(
            CONTRACT["migration"]["llm_dispatch"],
            "scoped_auxiliary_queue_timing_enrichment_by_dispatch_job_and_call_id",
        )
        self.assertIn("legacy LLM-call Parquet compatibility mirror", RUNTIME)
        self.assertIn("compatibility mirror", ACTIVATION)
        self.assertIn("WHERE legacy.llm_call_id IS NULL", READER)
        self.assertIn(
            "canonical.llm_call_id = legacy.llm_call_id", READER
        )
        self.assertIn(
            "fn compatibility_mirror_is_hidden_when_canonical_call_exists()",
            READER,
        )
        self.assertIn(
            "capture_status IN ('complete', 'metadata_only', 'redacted')",
            READER,
        )
        self.assertIn("fn install_dispatch_timing_view(", READER)
        self.assertIn("unclassified_transport_events_lost", READER)
        self.assertIn("UNCLASSIFIED_TRANSPORT_LAG_REASONS_SQL", READER)
        self.assertIn("known_missing_fact_revisions", READER)
        self.assertIn("KNOWN_MISSING_FACT_REASONS_SQL", READER)
        for reason in CONTRACT["coverage_accounting"]["known_missing_fact_revisions"]:
            self.assertIn(reason, READER)
        self.assertIn('body["data"]["known_missing_fact_revisions"]', API)
        self.assertIn("known missing fact revisions", UI)
        self.assertIn("transport envelopes unclassified", UI)
        self.assertIn("fn install_empty_dispatch_timing_view(", READER)
        self.assertIn("d.dispatch_job_id = a.dispatch_job_id", READER)
        self.assertIn("d.llm_call_id = a.llm_call_id", READER)
        self.assertIn(
            "fn scoped_dispatch_timing_enriches_queued_attempt_and_call()",
            READER,
        )

    def test_recovery_duplicate_saturation_partition_schema_and_shutdown_matrix_exists(self) -> None:
        required = (
            "journal_restart_replays_uncommitted_records_in_sequence_order",
            "duplicate_delivery_is_idempotent_and_conflicting_revision_fails_closed",
            "rotation_preserves_order_and_repairs_only_a_partial_final_line",
            "saturated_critical_buffer_accumulates_exact_gap_and_emits_on_recovery",
            "bounded_shutdown_drains_and_materializes_queued_records",
            "duplicate_replay_verifies_existing_objects_without_double_counting",
            "revisions_are_partitioned_by_utc_event_date_across_midnight",
            "canonical_schema_evolution_projects_missing_columns_as_typed_nulls",
            "legacy_rows_are_normalized_without_trusting_embedded_scope",
            "activation_restart_duplicate_and_shutdown_are_durable",
        )
        combined = JOURNAL + MATERIALIZER + READER + ACTIVATION
        for name in required:
            with self.subTest(test=name):
                self.assertIn(f"fn {name}()", combined)

    def test_retention_includes_all_fact_streams_and_excludes_journal(self) -> None:
        for stream in CONTRACT["retention"]["utc_partition_streams"]:
            self.assertIn(f'"{stream}"', RETENTION)
        self.assertIn(
            "fn retention_covers_canonical_fact_streams_but_never_journal_segments()",
            RETENTION,
        )
        self.assertIn("journal_old.exists()", RETENTION)

    def test_reconciliation_is_content_free_strict_and_reports_html(self) -> None:
        self.assertIn("def reconcile(", EVALUATOR)
        self.assertIn("canonical_to_legacy_match_rate", EVALUATOR)
        self.assertIn("queued_dispatch_join_rate", EVALUATOR)
        self.assertIn("attempt_accounting_complete", EVALUATOR)
        self.assertIn("attempt_accounting_violation_count", EVALUATOR)
        self.assertIn("malformed_attempt_gap_count", EVALUATOR)
        self.assertIn("captured_attempt_counts", EVALUATOR)
        self.assertIn("attempt_gap_counts", EVALUATOR)
        self.assertIn("embedded_scope_mismatch_count", EVALUATOR)
        self.assertIn("malformed_attempt_identity_count", EVALUATOR)
        self.assertIn("orphan_attempt_count", EVALUATOR)
        self.assertIn("duplicate_attempt_count", EVALUATOR)
        self.assertIn("duplicate_canonical_call_count", EVALUATOR)
        self.assertIn("duplicate_legacy_call_count", EVALUATOR)
        self.assertIn("canonical_missing_identity_count", EVALUATOR)
        self.assertIn('expected_attempt_id = f"{call_id}:a{attempt_index}"', EVALUATOR)
        self.assertEqual(
            CONTRACT["reconciliation"]["embedded_scope"],
            "every_selected_fact_must_match_the_requested_scope_except_process_lag_diagnostics_in_anonymous_default",
        )
        self.assertEqual(
            CONTRACT["reconciliation"]["provider_attempt_identity"],
            "llm_call_id_colon_a_one_based_provider_attempt_index_with_no_orphans_or_duplicates",
        )
        self.assertEqual(
            CONTRACT["reconciliation"]["terminal_call_identity"],
            "nonempty_and_unique_in_canonical_and_compatibility_mirror_cohorts",
        )
        self.assertEqual(
            CONTRACT["reconciliation"]["attempt_equation"],
            "for_each_llm_call_captured_terminal_attempts_plus_call_owned_explicit_unavailable_attempts_equals_provider_attempt_count",
        )
        self.assertIn("gap.llm_call_id = Some(llm_call_id.to_string());", ACTIVATION)
        self.assertIn("row.llm_call_id.clone_from(&record.llm_call_id);", MATERIALIZER)
        self.assertIn("math.isclose", EVALUATOR)
        self.assertIn("def typed_projection(", EVALUATOR)
        self.assertIn('CAST(NULL AS {FIELD_TYPES[field]})', EVALUATOR)
        self.assertIn("def read_overview_api(", EVALUATOR)
        self.assertIn("content_fields_read\": []", EVALUATOR)
        for forbidden in (
            "prompt",
            "response",
            "tool_arguments",
            "tool_results",
            "transcript",
        ):
            self.assertNotIn(f'"{forbidden}",\n    "llm_call_id"', EVALUATOR)
        self.assertIn("report.html", EVALUATOR)

    def test_live_eval_is_final_and_its_child_report_reaches_summary(self) -> None:
        memory = LIVE_RUNNER.index("\nrun_memory_temperature_eval\n")
        phase2f = LIVE_RUNNER.index("\nrun_llm_phase2f_eval\n", memory)
        summary = LIVE_RUNNER.index("\nended_epoch=", phase2f)
        self.assertLess(memory, phase2f)
        self.assertLess(phase2f, summary)
        self.assertIn('--suite "LLM observability Phase 2F"', LIVE_RUNNER)
        self.assertIn('"$llm_phase2f_report"', LIVE_RUNNER)
        self.assertIn("llm-trace-phase2f-audit:", MAKEFILE)
        self.assertIn("test-llm-trace-phase2f:", MAKEFILE)


if __name__ == "__main__":
    unittest.main()

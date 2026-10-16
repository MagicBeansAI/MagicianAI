#!/usr/bin/env python3
"""Provider-free structural regressions for Phase 4 tool/action lineage."""

from __future__ import annotations

import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = json.loads(
    (ROOT / "data/magician_v2/llm_observability/phase4-tool-lineage-contract-v1.json").read_text(
        encoding="utf-8"
    )
)
ANALYTICS = ROOT / "magician/src/magician_v2/analytics"
RECORDER = (ANALYTICS / "llm_trace_recorder.rs").read_text(encoding="utf-8")
LINEAGE = (ANALYTICS / "llm_tool_lineage.rs").read_text(encoding="utf-8")
ACTIVATION = (ANALYTICS / "llm_trace_activation.rs").read_text(encoding="utf-8")
JOURNAL = (ANALYTICS / "llm_trace_journal.rs").read_text(encoding="utf-8")
MATERIALIZER = (ANALYTICS / "llm_trace_materializer.rs").read_text(encoding="utf-8")
REGISTRY = (ANALYTICS / "llm_fact_registry.rs").read_text(encoding="utf-8")
WORKSPACE = (
    ROOT / "magician/src/magician_v2/artifact_v2/workspace.rs"
).read_text(encoding="utf-8")
READER = (ANALYTICS / "llm_analytics_read_service.rs").read_text(encoding="utf-8")
CHAT = (ROOT / "magician/src/magician_v2/chat/service.rs").read_text(encoding="utf-8")
AGENTIC = (ROOT / "magician/src/magician_v2/execution/agentic/executor.rs").read_text(
    encoding="utf-8"
)
AGENTIC_TYPES = (
    ROOT / "magician/src/magician_v2/execution/agentic/types.rs"
).read_text(encoding="utf-8")
INTERNAL_DATA = (
    ROOT / "magician/src/magician_v2/execution/internal_data_provider.rs"
).read_text(encoding="utf-8")
INTERNAL_DATA_YAML = (
    ROOT / "magician/src/magician_v2/execution/embedded_pack_defs/internal_data.yaml"
).read_text(encoding="utf-8")
API = (ROOT / "magician-api/src/analytics_api.rs").read_text(encoding="utf-8")
RUNTIME = (ROOT / "magician-bin/src/main.rs").read_text(encoding="utf-8")
UI = (
    ROOT / "ui/unified-ui/src/lib/magician/llm/LlmCallExplorer.svelte"
).read_text(encoding="utf-8")
UI_ROUTE = (ROOT / "ui/unified-ui/src/routes/(app)/llm/+page.svelte").read_text(
    encoding="utf-8"
)
EVALUATOR = (ROOT / "scripts/eval-llm-observability-phase4.py").read_text(encoding="utf-8")
MAKEFILE = (ROOT / "Makefile").read_text(encoding="utf-8")
LIVE_RUNNER = (ROOT / "scripts/run-live-evals-with-report.sh").read_text(encoding="utf-8")


class Phase4ToolLineageContractTests(unittest.TestCase):
    def test_contract_has_complete_stable_lifecycle(self) -> None:
        for stage in CONTRACT["stages"]:
            rust_variant = "".join(part.title() for part in stage.split("_"))
            self.assertIn(rust_variant, RECORDER)
        for field in CONTRACT["independent_classifications"]:
            self.assertIn(field, RECORDER)
        self.assertIn("stage.revision(record.stage_index)", RECORDER)
        self.assertIn("arguments fingerprint at every stage", RECORDER)
        self.assertTrue(CONTRACT["semantic_invariants"]["result_validation_requires_actual_result"])
        self.assertEqual(
            CONTRACT["semantic_invariants"]["transport_failure_side_effect_state"],
            "unknown_until_authoritative_rollback",
        )

    def test_fact_lane_is_content_free_scoped_and_durable(self) -> None:
        for marker in (
            "scoped_content_fingerprint",
            "canonical_json_value",
            "LLM_TOOL_LINEAGE_EVENT_TYPE",
            "record_tool_lineage",
            "LlmCanonicalDataset::ToolCalls",
            "analytics_llm_tool_calls_root",
            '"llm_tool_calls"',
        ):
            self.assertIn(
                marker,
                "\n".join((LINEAGE, ACTIVATION, MATERIALIZER, REGISTRY, WORKSPACE)),
            )
        fact_columns = MATERIALIZER[
            MATERIALIZER.index("pub const FACT_COLUMNS") : MATERIALIZER.index(
                "pub const RESTRICTED_CONTENT_COLUMNS"
            )
        ]
        for forbidden in ("tool_arguments_json", "tool_result_json", "restricted_payload_json"):
            self.assertNotIn(forbidden, fact_columns)
        self.assertFalse(CONTRACT["identity"]["payload_content_in_fact_dataset"])

    def test_chat_and_agentic_paths_emit_the_same_contract(self) -> None:
        for marker in (
            "emit_tool_proposed",
            "emit_completed_tool_dispatch",
            "emit_tool_branch_materialized",
        ):
            self.assertIn(marker, CHAT)
        # Chat has no phase address and emits directly. The agentic run loop
        # builds the same lineage envelopes and sends them through its
        # journal-and-emit rail so recovery cannot lose the durable record.
        for marker in (
            "tool_proposed_envelope",
            "completed_tool_dispatch_envelopes",
            "tool_branch_materialized_envelope",
        ):
            self.assertIn(marker, AGENTIC)
        self.assertIn("emit_tool_result_consumed", CHAT)
        self.assertIn("emit_agentic_tool_consumption", AGENTIC)
        self.assertIn("observe_tool_recovery", CHAT)
        self.assertIn("observe_tool_recovery", AGENTIC)
        self.assertIn("tool_lineage_producing_call_trace_receipt_missing", AGENTIC)
        self.assertIn("tool_rollback_envelopes", AGENTIC)
        self.assertIn("journal_and_emit_at", AGENTIC)
        self.assertIn("agentic_tool_rollback_outcome", AGENTIC)
        self.assertIn("llm_trace_context", AGENTIC_TYPES)
        self.assertIn("model_tool_call_id", CHAT)
        self.assertIn("tool_execution_id", CHAT)

    def test_governed_trace_reads_have_one_shared_assembler(self) -> None:
        for marker in ("list_traces_envelope", "read_trace_envelope", "tool_timeline"):
            self.assertIn(marker, READER)
        for action in CONTRACT["governed_reads"]["internal_data"]:
            self.assertIn(action, INTERNAL_DATA)
            self.assertIn(action, INTERNAL_DATA_YAML)
        for route in ("/analytics/llm/traces", "/analytics/llm/traces/{trace_id}"):
            self.assertIn(route, RUNTIME)
        self.assertIn("list_llm_traces_handler", API)
        self.assertIn("read_llm_trace_handler", API)
        self.assertIn("LlmCallExplorer", UI_ROUTE)
        self.assertIn("tool_timeline", UI)

    def test_authored_matrix_covers_phase4_failure_and_edge_cases(self) -> None:
        combined = "\n".join((RECORDER, LINEAGE, ACTIVATION, JOURNAL, MATERIALIZER, READER, INTERNAL_DATA, API, AGENTIC))
        for test_name in (
            "tool_lineage_revision_ranges_keep_attempts_consumers_and_rollbacks_distinct",
            "tool_lineage_validates_independent_parse_policy_approval_and_transport_outcomes",
            "tool_result_consumption_is_an_explicit_repeatable_edge",
            "abandoned_branch_and_rollback_are_not_conflated_with_tool_failure",
            "cross_execution_delegation_ids_are_fact_only_and_stage_bound",
            "tool_lineage_rejects_invented_execution_identity_and_duplicate_children",
            "classification_keeps_policy_approval_and_tool_failures_separate",
            "multiple_and_parallel_tool_calls_keep_distinct_provider_identities",
            "recovery_requires_an_earlier_authoritative_transport_failure",
            "failed_transport_never_invents_absence_of_side_effects",
            "related_execution_ids_are_deduplicated_bounded_and_content_free",
            "tool_rollback_lineage_uses_only_authoritative_runtime_markers",
            "tool_lineage_scope_mismatch_becomes_an_explicit_capture_gap",
            "declared_agentic_mapping_gap_preserves_machine_reason_and_call_owner",
            "saturated_lineage_buffer_emits_call_owned_capture_gap",
            "tool_lifecycle_materializes_as_ordered_content_free_revisions",
            "shared_trace_assembler_returns_ordered_calls_attempts_and_tool_stages",
            "governed_tool_lineage_rejects_producing_call_context_drift",
            "governed_trace_handlers_share_scope_and_absence_semantics",
        ):
            self.assertIn(f"fn {test_name}", combined)

    def test_live_exit_audit_is_bounded_fact_only_and_aggregated(self) -> None:
        for marker in (
            "MAX_AUDIT_ROWS",
            "execution_missing_authoritative_outcome_or_gap",
            "execution_missing_branch_or_gap",
            "successful_branch_missing_execution_or_validation",
            "related_execution_ids_on_wrong_stage",
            "unowned_lineage_or_transport_capture_loss",
            "invalid_tool_execution_identity",
            "result_consumed_by_producing_call",
            '"stable_ids_written_to_report": False',
            '"payloads_read": False',
        ):
            self.assertIn(marker, EVALUATOR)
        rendered = EVALUATOR[EVALUATOR.index("def render_html"):EVALUATOR.index("def write_report")]
        for stable_id in ("trace_id", "llm_call_id", "model_tool_call_id", "tool_execution_id"):
            self.assertNotIn(stable_id, rendered)

    def test_phase4_is_a_focused_target_and_final_live_report_child(self) -> None:
        self.assertIn("test-llm-trace-phase4:", MAKEFILE)
        self.assertIn("llm-trace-phase4-audit:", MAKEFILE)
        self.assertIn('--suite "LLM observability Phase 4"', LIVE_RUNNER)
        phase3 = LIVE_RUNNER.index("\nrun_llm_phase3_eval\n")
        phase4 = LIVE_RUNNER.index("\nrun_llm_phase4_eval\n", phase3)
        summary = LIVE_RUNNER.index("\nended_epoch=", phase4)
        self.assertLess(phase3, phase4)
        self.assertLess(phase4, summary)


if __name__ == "__main__":
    unittest.main()

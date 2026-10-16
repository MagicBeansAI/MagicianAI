#!/usr/bin/env python3
"""Structural regressions for ordering and reporting of the live-eval aggregate."""

from __future__ import annotations

from pathlib import Path
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]
RUNNER = REPO_ROOT / "scripts" / "run-live-evals-with-report.sh"


class RunLiveEvalsOrderingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = RUNNER.read_text(encoding="utf-8")

    def test_make_can_pin_the_python_interpreter_for_third_party_dependencies(self) -> None:
        self.assertIn('python_bin="${LIVE_EVAL_PYTHON:-python3}"', self.source)
        self.assertIn('"$python_bin" "$repo_root/$script"', self.source)
        self.assertNotIn('python3 "$repo_root/', self.source)

    def test_memory_temperature_is_last_cost_bearing_and_phase4_lineage_audit_is_final(self) -> None:
        retrieval = self.source.index("\nrun_chat_context_retrieval_eval\n")
        memory = self.source.index("\nrun_memory_temperature_eval\n", retrieval)
        phase2f = self.source.index("\nrun_llm_phase2f_eval\n", memory)
        phase3 = self.source.index("\nrun_llm_phase3_eval\n", phase2f)
        phase4 = self.source.index("\nrun_llm_phase4_eval\n", phase3)
        summary = self.source.index("\nended_epoch=", phase4)
        self.assertLess(retrieval, memory)
        self.assertLess(memory, phase2f)
        self.assertLess(phase2f, phase3)
        self.assertLess(phase3, phase4)
        self.assertLess(phase4, summary)
        self.assertNotIn("\nrun_eval ", self.source[memory:phase2f])

    def test_memory_temperature_has_child_report_and_failure_gate(self) -> None:
        self.assertIn('--suite "Memory temperature"', self.source)
        self.assertIn('"$memory_temperature_status"', self.source)
        self.assertIn('eval_report="$output_dir/report.html"', self.source)

    def test_tool_projection_context_lane_precedes_final_local_model_and_is_reported(self) -> None:
        retrieval = self.source.index("\nrun_chat_context_retrieval_eval\n")
        projection = self.source.index("\nrun_tool_result_projection_context_eval\n", retrieval)
        memory = self.source.index("\nrun_memory_temperature_eval\n", projection)
        self.assertLess(retrieval, projection)
        self.assertLess(projection, memory)
        self.assertIn('--suite "Tool-result projection and staged context"', self.source)
        self.assertIn('"$tool_projection_status"', self.source)
        self.assertIn("test-tool-result-projection-context-eval-harness", self.source)
        self.assertIn("test-tool-result-projection-context-live-eval", self.source)

    def test_provider_replay_is_cross_provider_reported_and_failure_gated(self) -> None:
        projection = self.source.index("\nrun_tool_result_projection_context_eval\n")
        replay = self.source.index("\nrun_provider_replay_eval\n", projection)
        memory = self.source.index("\nrun_memory_temperature_eval\n", replay)
        self.assertLess(projection, replay)
        self.assertLess(replay, memory)
        self.assertIn('local slug="provider-replay"', self.source)
        self.assertIn("test-provider-replay-eval-harness", self.source)
        self.assertIn("test-provider-replay-live-eval", self.source)
        self.assertIn('--suite "Provider replay"', self.source)
        failure_gate = self.source[self.source.index("for status in ") :]
        self.assertIn('"$provider_replay_status"', failure_gate)
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn("provider-replay", self.source[allowlist_start:allowlist_end])

    def test_preplan_lifecycle_is_reported_gated_and_precedes_final_cost_lane(self) -> None:
        replay = self.source.index("\nrun_provider_replay_eval\n")
        preplan = self.source.index(
            '\nrun_eval "Pre-plan lifecycle" "preplan-flow"', replay
        )
        memory = self.source.index("\nrun_memory_temperature_eval\n", preplan)
        self.assertLess(replay, preplan)
        self.assertLess(preplan, memory)
        self.assertIn('--suite "Pre-plan lifecycle"', self.source)
        failure_gate = self.source[self.source.index("for status in ") :]
        self.assertIn('"$preplan_status"', failure_gate)
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn("preplan-flow", self.source[allowlist_start:allowlist_end])
        self.assertIn("PREPLAN_LIVE_HITL_MODE", self.source)
        self.assertIn(
            'if [[ "$slug" == "preplan-flow" || "$slug" == "web-researcher" ]]',
            self.source,
        )
        self.assertIn('if [[ -f "$output_dir/report.html" ]]', self.source)

    def test_web_researcher_direct_and_delegated_lane_is_reported(self) -> None:
        preplan = self.source.index(
            '\nrun_eval "Pre-plan lifecycle" "preplan-flow"'
        )
        researcher = self.source.index(
            '\nrun_eval "Web researcher" "web-researcher"', preplan
        )
        memory = self.source.index("\nrun_memory_temperature_eval\n", researcher)
        self.assertLess(preplan, researcher)
        self.assertLess(researcher, memory)
        self.assertIn("WEB_RESEARCHER_LIVE_RUNS", self.source)
        self.assertIn('--suite "Web researcher"', self.source)
        failure_gate = self.source[self.source.index("for status in ") :]
        self.assertIn('"$web_researcher_status"', failure_gate)
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn("web-researcher", self.source[allowlist_start:allowlist_end])
        self.assertIn(
            'if [[ "$slug" == "preplan-flow" || "$slug" == "web-researcher" ]]',
            self.source,
        )

    def test_memory_temperature_can_run_in_isolation(self) -> None:
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn("memory-temperature", self.source[allowlist_start:allowlist_end])

    def test_phase2f_has_child_report_failure_gate_and_isolation(self) -> None:
        self.assertIn('--suite "LLM observability Phase 2F"', self.source)
        self.assertIn('"$llm_phase2f_status"', self.source)
        self.assertIn('eval-llm-observability-phase2f.py" --self-test', self.source)
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn(
            "llm-observability-phase2f",
            self.source[allowlist_start:allowlist_end],
        )

    def test_phase3_has_child_report_skip_gate_and_isolation(self) -> None:
        self.assertIn('--suite "LLM observability Phase 3"', self.source)
        self.assertIn('"$llm_phase3_status"', self.source)
        self.assertIn('eval-llm-observability-phase3.py" --self-test', self.source)
        self.assertIn('if [[ $eval_status -eq 3 ]]', self.source)
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn(
            "llm-observability-phase3",
            self.source[allowlist_start:allowlist_end],
        )

    def test_phase4_has_child_report_skip_gate_and_isolation(self) -> None:
        self.assertIn('--suite "LLM observability Phase 4"', self.source)
        self.assertIn('"$llm_phase4_status"', self.source)
        self.assertIn('eval-llm-observability-phase4.py" --self-test', self.source)
        self.assertIn('if [[ $eval_status -eq 3 ]]', self.source)
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn(
            "llm-observability-phase4",
            self.source[allowlist_start:allowlist_end],
        )

    def test_content_retrieval_runtime_is_reported_gated_and_runs_before_final_audits(self) -> None:
        compactor = self.source.index("\nrun_compactor_patch_validity_eval\n")
        runtime = self.source.index(
            "\nrun_content_retrieval_runtime_eval\n", compactor
        )
        phase2f = self.source.index("\nrun_llm_phase2f_eval\n", runtime)
        self.assertLess(compactor, runtime)
        self.assertLess(runtime, phase2f)
        self.assertIn('--suite "Content retrieval runtime"', self.source)
        self.assertIn('"$content_retrieval_runtime_status"', self.source)
        self.assertIn('eval_report="$report"', self.source)

    def test_runtime_content_retrieval_is_independently_reported_and_gated(self) -> None:
        self.assertIn('--suite "Content retrieval runtime"', self.source)
        self.assertIn('"$content_retrieval_runtime_status"', self.source)
        self.assertIn('"$content_retrieval_runtime_report"', self.source)
        failure_gate = self.source[self.source.index("for status in ") :]
        self.assertIn('"$content_retrieval_runtime_status"', failure_gate)

    def test_runtime_content_retrieval_supports_isolation_self_test_and_dry_run(self) -> None:
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn(
            "content-retrieval-runtime",
            self.source[allowlist_start:allowlist_end],
        )
        function_start = self.source.index("run_content_retrieval_runtime_eval()")
        function_end = self.source.index("\nrun_llm_phase2f_eval()", function_start)
        function = self.source[function_start:function_end]
        # The aggregate self-test owns one canonical retrieval lane. Its full
        # provider-free harness includes the runtime self-test as its final
        # gate, so invoking the narrower target here would lose source/browser
        # coverage and duplicate orchestration knowledge.
        self.assertIn("test-content-retrieval-eval-harness", function)
        self.assertIn("test-content-retrieval-runtime-live-eval", function)
        self.assertIn("CONTENT_RETRIEVAL_RUNTIME_LIVE_EVAL_ARGS=--dry-run", function)
        self.assertIn('eval_report="$report"', function)

    def test_observable_sources_is_isolated_reported_and_failure_gated(self) -> None:
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn("observable-sources", self.source[allowlist_start:allowlist_end])
        function_start = self.source.index("run_observable_sources_eval()")
        function_end = self.source.index("\nrun_llm_phase2f_eval()", function_start)
        function = self.source[function_start:function_end]
        self.assertIn("test-observable-sources-eval-harness", function)
        self.assertIn("test-observable-sources-live-eval", function)
        self.assertIn("--dry-run", function)
        self.assertIn('eval_report="$report"', function)
        self.assertIn('--suite "Observable sources"', self.source)
        failure_gate = self.source[self.source.index("for status in ") :]
        self.assertIn('"$observable_sources_status"', failure_gate)

    def test_storage_governance_is_isolated_reported_and_failure_gated(self) -> None:
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        self.assertIn("storage-governance", self.source[allowlist_start:allowlist_end])
        function_start = self.source.index("run_storage_governance_eval()")
        function_end = self.source.index("\nrun_llm_phase2f_eval()", function_start)
        function = self.source[function_start:function_end]
        self.assertIn("test-storage-governance-eval-harness", function)
        self.assertIn("test-storage-governance-live-eval", function)
        self.assertIn("--dry-run", function)
        self.assertIn('eval_report="$output_dir/report.html"', function)
        self.assertIn('--suite "Storage governance"', self.source)
        failure_gate = self.source[self.source.index("for status in ") :]
        self.assertIn('"$storage_governance_status"', failure_gate)

    def test_content_retrieval_has_one_canonical_live_lane(self) -> None:
        allowlist_start = self.source.index('case "$only_eval" in')
        allowlist_end = self.source.index("esac", allowlist_start)
        allowlist = self.source[allowlist_start:allowlist_end]
        self.assertIn("content-retrieval-runtime", allowlist)
        self.assertNotIn("|content-retrieval|", allowlist)
        self.assertNotIn("run_content_retrieval_eval()", self.source)
        self.assertEqual(
            self.source.count("\nrun_content_retrieval_runtime_eval\n"),
            1,
        )

    def test_compactor_live_lane_gates_semantic_and_protected_information(self) -> None:
        function_start = self.source.index("run_compactor_patch_validity_eval()")
        function_end = self.source.index(
            "\nrun_content_retrieval_runtime_eval()", function_start
        )
        function = self.source[function_start:function_end]
        self.assertIn("COMPACTOR_MIN_SEMANTIC_RETENTION", function)
        self.assertIn("COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION", function)
        self.assertIn("test-compactor-patch-validity-eval-harness", function)
        self.assertIn("pre/post-budget semantic retention", self.source)
        self.assertIn("strict protected-information gates", self.source)


if __name__ == "__main__":
    unittest.main()

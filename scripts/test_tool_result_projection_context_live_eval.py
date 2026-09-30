"""Provider-free contract tests for the focused projection/context live lane."""

from __future__ import annotations

import importlib.util
import statistics
import sys
import tempfile
import unittest
import json
from dataclasses import asdict
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/eval-tool-result-projection-context-live.py"


def load_module():
    spec = importlib.util.spec_from_file_location("tool_projection_eval_under_test", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ToolResultProjectionContextLiveEvalTests(unittest.TestCase):
    def test_matrix_covers_every_required_surface_and_failure_family(self) -> None:
        module = load_module()
        items = module.scenarios()
        self.assertEqual(len(items), 17)
        self.assertEqual({item.surface for item in items}, {"chat", "realtime_voice", "autonomous_task"})
        names = {item.name for item in items}
        for required in (
            "owner_fact_beyond_prefix",
            "shared_meeting_private_denial",
            "procedure_timeout_memory_survives",
            "memory_timeout_procedure_survives",
            "continuation_omitted_record",
            "reference_revoked",
            "realtime_rotation_replay",
            "multi_tool_reuse",
            "ambiguous_domain_fields_survive",
            "null_error_is_not_failure",
        ):
            self.assertIn(required, names)

    def test_second_record_failure_fixture_is_absent_from_legacy_voice_prefix(self) -> None:
        module = load_module()
        item = next(value for value in module.scenarios() if value.name == "owner_fact_beyond_prefix")
        self.assertNotIn("14 September", str(module.legacy_view(item)))
        self.assertIn("14 September", str(item.projected))

    def test_live_runner_and_makefile_publish_the_lane(self) -> None:
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        runner = (ROOT / "scripts/run-live-evals-with-report.sh").read_text(encoding="utf-8")
        self.assertIn("test-tool-result-projection-context-live-eval", makefile)
        self.assertIn("tool-result-projection-context", runner)
        self.assertIn("Tool-result projection and staged context", runner)
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("tool_result_projection_context_live_fixtures", source)
        self.assertIn("generate_runtime_fixtures", source)
        self.assertIn("TOOL_RESULT_PROJECTION_LIVE_SCENARIOS", makefile)

    def test_runtime_fixture_request_covers_projection_read_revocation_and_staged_deadlines(self) -> None:
        module = load_module()
        payload = module.runtime_fixture_input(list(module.scenarios()))
        cases = {case["name"]: case for case in payload["cases"]}
        self.assertEqual(len(cases), 17)
        self.assertEqual(cases["owner_fact_beyond_prefix"]["contract_id"], "ranked_records_v1")
        self.assertEqual(
            cases["queued_task_receipt"]["expected_outcome_status"],
            "pending",
        )
        self.assertEqual(
            cases["owner_fact_beyond_prefix"]["question"],
            "What is the wife's birthday?",
        )
        self.assertTrue(cases["continuation_omitted_record"]["continuation_probe"])
        self.assertTrue(cases["reference_revoked"]["revocation_probe"])
        self.assertEqual(
            cases["procedure_timeout_memory_survives"]["staged"]["procedures"]["state"],
            "pending",
        )

    def test_scoring_requires_exact_status_and_evidence_presence(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(value for value in module.scenarios() if value.name == "unrelated_next_turn")
        response = module.fake_response(item.expected_answer, "not ok")
        result = module.score(
            helpers,
            item,
            "projected",
            1,
            200,
            response,
            10,
            4,
            6,
            None,
            None,
        )
        self.assertFalse(result.status_pass)
        self.assertFalse(result.passed)

    def test_projected_matrix_expects_production_normalized_statuses(self) -> None:
        module = load_module()
        expected = {item.name: item.expected_status for item in module.scenarios()}
        self.assertEqual(expected["owner_fact_record_1"], "succeeded")
        self.assertEqual(expected["shared_meeting_private_denial"], "denied")
        self.assertEqual(expected["queued_task_receipt"], "pending")
        self.assertEqual(expected["long_tool_error"], "failed")
        self.assertEqual(expected["reference_revoked"], "revoked")
        self.assertEqual(expected["procedure_timeout_memory_survives"], "partial")
        self.assertEqual(expected["ambiguous_domain_fields_survive"], "succeeded")
        self.assertEqual(expected["null_error_is_not_failure"], "succeeded")

    def test_grading_tool_restricts_status_to_typed_runtime_vocabulary(self) -> None:
        module = load_module()
        status = module.tool()["parameters"]["properties"]["status"]
        self.assertEqual(
            set(status["enum"]),
            {
                "succeeded",
                "partial",
                "failed",
                "denied",
                "cancelled",
                "pending",
                "requires_approval",
                "timed_out",
                "revoked",
                "unknown",
            },
        )
        self.assertNotIn("ok", status["enum"])
        self.assertNotIn("error", status["enum"])

    def test_live_jobs_are_adjacent_seeded_and_exactly_counterbalanced_at_six_runs(self) -> None:
        module = load_module()
        items = list(module.scenarios())
        jobs = module.build_jobs(items, 6, 7272026)
        self.assertEqual(jobs, module.build_jobs(items, 6, 7272026))
        self.assertEqual(len(jobs), len(items) * 12)
        for offset in range(0, len(jobs), 2):
            first, second = jobs[offset : offset + 2]
            self.assertEqual(first[0], second[0])
            self.assertEqual(first[1].name, second[1].name)
            self.assertEqual({first[2], second[2]}, {"legacy", "projected"})
        for item in items:
            first_positions = [
                jobs[offset][2]
                for offset in range(0, len(jobs), 2)
                if jobs[offset][1].name == item.name
            ]
            self.assertEqual(first_positions.count("legacy"), 3)
            self.assertEqual(first_positions.count("projected"), 3)

    def test_percentile_uses_standard_linear_interpolation_not_sample_maximum(self) -> None:
        module = load_module()
        self.assertAlmostEqual(module.percentile(list(range(1, 18)), 0.95), 16.2)

    def test_exact_answer_matching_rejects_numeric_substrings(self) -> None:
        module = load_module()
        self.assertTrue(module.contains_expected_answer("The price is 70.", "70"))
        self.assertFalse(module.contains_expected_answer("The price is 170.", "70"))
        self.assertTrue(
            module.contains_expected_answer("The code was upstream timeout.", "upstream_timeout")
        )
        self.assertTrue(
            module.contains_expected_answer(
                "Region ap-south-1, channel canary, owner platform-team.",
                "ap-south-1 canary platform-team",
            )
        )
        queued = next(
            item for item in module.scenarios() if item.name == "queued_task_receipt"
        )
        self.assertIn("no", queued.accepted_answers)
        revoked = next(
            item for item in module.scenarios() if item.name == "reference_revoked"
        )
        self.assertIn("authority revision changed", revoked.accepted_answers)

    def test_privacy_cases_accept_safe_denial_phrasings_but_not_the_secret(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(
            value for value in module.scenarios() if value.name == "shared_meeting_private_denial"
        )
        response = module.fake_response("Access denied for this meeting.", "denied")
        result = module.score(
            helpers,
            item,
            "projected",
            1,
            200,
            response,
            10,
            4,
            6,
            None,
            None,
        )
        self.assertTrue(result.exact_pass)
        self.assertTrue(result.privacy_pass)
        self.assertTrue(result.passed)

    def test_paired_latency_gate_uses_scenario_medians_not_unpaired_call_outliers(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(value for value in module.scenarios() if value.name == "unrelated_next_turn")

        def scored(variant: str, run: int, latency: int):
            return module.score(
                helpers,
                item,
                variant,
                run,
                200,
                module.fake_response(item.expected_answer, item.expected_status),
                latency,
                latency,
                latency,
                None,
                None,
            )

        rows = [scored("legacy", run, value) for run, value in enumerate((100, 100, 100, 100, 2_000), 1)]
        rows += [scored("projected", run, 110) for run in range(1, 6)]
        paired = module.paired_scenario_latency_summary(rows, "total_ms")
        self.assertAlmostEqual(paired["p95_ratio"], 1.1)
        summary = module.summarize(rows, {item.name: item})
        self.assertFalse(
            any("paired scenario-median" in failure for failure in module.gate_failures(rows, summary))
        )

    def test_counterbalanced_latency_uses_variant_medians_over_matched_runs(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(value for value in module.scenarios() if value.name == "owner_fact_record_1")

        def scored(variant: str, run: int, latency: int):
            return module.score(
                helpers,
                item,
                variant,
                run,
                200,
                module.fake_response(item.expected_answer, item.expected_status),
                latency,
                latency,
                latency,
                None,
                None,
            )

        legacy = (1807, 1163, 667, 718, 594)
        projected = (1155, 1676, 726, 3509, 562)
        rows = [scored("legacy", run, value) for run, value in enumerate(legacy, 1)]
        rows += [
            scored("projected", run, value)
            for run, value in enumerate(projected, 1)
        ]
        paired = module.paired_scenario_latency_summary(rows, "total_ms")
        expected = statistics.median(projected) / statistics.median(legacy)
        self.assertAlmostEqual(paired["p95_ratio"], expected)
        self.assertEqual(paired["scenario_pair_counts"][item.name], 5)
        self.assertGreater(paired["p95_ratio"], 1.15)

    def test_live_prompt_bounds_answer_verbosity_for_latency_parity(self) -> None:
        module = load_module()
        self.assertIn("at most 12 words", module.INSTRUCTIONS)
        self.assertIn("including its\n`code` or `message`", module.INSTRUCTIONS)
        self.assertIn("separate `status` field", module.INSTRUCTIONS)
        self.assertIn("Do not\nexplain or restate", module.INSTRUCTIONS)

    def test_focused_scenario_gate_does_not_fail_unselected_surfaces(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(
            value
            for value in module.scenarios()
            if value.name == "ambiguous_domain_fields_survive"
        )
        rows = [
            module.score(
                helpers,
                item,
                variant,
                1,
                200,
                module.fake_response(item.expected_answer, item.expected_status),
                100,
                50,
                50,
                None,
                None,
            )
            for variant in ("legacy", "projected")
        ]
        summary = module.summarize(rows, {item.name: item})
        self.assertEqual(summary["surfaces"]["realtime_voice"]["cases"], 0)
        self.assertFalse(summary["full_performance_matrix"])
        self.assertFalse(module.gate_failures(rows, summary))
        self.assertIn("Focused scenario selection", module.focused_run_limitation(summary))

    def test_focused_scenario_skips_only_full_matrix_performance_gates(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(
            value
            for value in module.scenarios()
            if value.name == "ambiguous_domain_fields_survive"
        )

        def scored(variant: str, latency: int):
            return module.score(
                helpers,
                item,
                variant,
                1,
                200,
                module.fake_response(item.expected_answer, item.expected_status),
                latency,
                latency,
                latency,
                None,
                None,
            )

        rows = [scored("legacy", 100), scored("projected", 10_000)]
        summary = module.summarize(rows, {item.name: item})
        failures = module.gate_failures(rows, summary)
        self.assertGreater(summary["paired_latency"]["total_ms"]["p95_ratio"], 1.15)
        self.assertFalse(any("paired scenario-median" in failure for failure in failures))

    def test_focused_large_result_subset_does_not_apply_aggregate_token_gate(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(
            value for value in module.scenarios() if value.name == "large_table_complete_rows"
        )
        rows = []
        for variant in ("legacy", "projected"):
            row = module.score(
                helpers,
                item,
                variant,
                1,
                200,
                module.fake_response(item.expected_answer, item.expected_status),
                100,
                50,
                50,
                None,
                None,
            )
            row.input_tokens = 100 if variant == "legacy" else 1_000
            rows.append(row)
        summary = module.summarize(rows, {item.name: item})
        self.assertLess(summary["large_result_input_token_reduction_pct"], 0)
        self.assertFalse(
            any("input-token reduction" in failure for failure in module.gate_failures(rows, summary))
        )

    def test_complete_matrix_retains_performance_gates(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        items = module.scenarios()
        rows = []
        for item in items:
            for variant, latency in (("legacy", 100), ("projected", 10_000)):
                rows.append(
                    module.score(
                        helpers,
                        item,
                        variant,
                        1,
                        200,
                        module.fake_response(item.expected_answer, item.expected_status),
                        latency,
                        latency,
                        latency,
                        None,
                        None,
                    )
                )
        summary = module.summarize(rows, {item.name: item for item in items})
        self.assertTrue(summary["full_performance_matrix"])
        self.assertIsNone(module.focused_run_limitation(summary))
        self.assertTrue(
            any(
                "paired scenario-median" in failure
                for failure in module.gate_failures(rows, summary)
            )
        )

    def test_direct_live_make_target_defaults_to_ssd_report_tree(self) -> None:
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        self.assertIn(
            "TOOL_RESULT_PROJECTION_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/tool-result-projection-context/live/latest",
            makefile,
        )

    def test_stored_live_report_can_be_rescored_without_provider_calls(self) -> None:
        module = load_module()
        helpers = module.load_helpers(ROOT)
        item = next(value for value in module.scenarios() if value.name == "queued_task_receipt")
        row = module.score(
            helpers,
            item,
            "projected",
            1,
            200,
            module.fake_response("pending", "pending"),
            100,
            50,
            50,
            None,
            None,
        )
        row.exact_pass = False
        row.passed = False
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source.json"
            output = Path(temp) / "rescored"
            source.write_text(
                json.dumps({"results": [asdict(row)], "runtime_fixtures": {"cases": []}}),
                encoding="utf-8",
            )
            rescored = module.rescore_report(source, output)
            self.assertTrue(rescored["results"][0]["exact_pass"])
            self.assertTrue(rescored["results"][0]["passed"])
            self.assertEqual(rescored["provider_calls_reused"], 1)
            self.assertTrue((output / "report.html").is_file())

    def test_reports_include_machine_readable_jsonl(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('output / "results.jsonl"', source)
        self.assertIn("JSONL results", source)

    def test_surface_services_do_not_select_projection_contracts_by_tool_or_family_name(self) -> None:
        for relative in (
            "magician/src/magician_v2/chat/service.rs",
            "magician-media/src/media_rails/voice_orchestrator.rs",
            "magician/src/magician_v2/execution/agentic/executor.rs",
        ):
            source = (ROOT / relative).read_text(encoding="utf-8")
            for contract_id in (
                "ranked_records_v1",
                "tabular_rows_v1",
                "document_spans_v1",
                "artifact_manifest_v1",
                "task_receipt_v1",
                "scalar_or_object_v1",
            ):
                self.assertNotIn(contract_id, source, f"{relative} hardcodes {contract_id}")

    def test_surface_services_forward_complete_capability_projection_metadata(self) -> None:
        chat = (ROOT / "magician/src/magician_v2/chat/service.rs").read_text(encoding="utf-8")
        autonomous = (
            ROOT / "magician/src/magician_v2/execution/agentic/executor.rs"
        ).read_text(encoding="utf-8")
        runtime = (ROOT / "magician/src/magician_v2/tool_result_runtime.rs").read_text(
            encoding="utf-8"
        )
        for source in (chat, autonomous):
            self.assertNotIn(".map(|contract| &contract.contract_id)", source)
            self.assertIn("entry.result_projection.as_ref()", source)
        self.assertIn("registry.register_override(contract.clone())", runtime)

    def test_retired_character_prefix_paths_are_absent_from_model_evidence(self) -> None:
        voice = (ROOT / "magician-media/src/media_rails/voice_orchestrator.rs").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("VOICE_TOOL_FULL_RESULT_PREVIEW_MAX_CHARS", voice)
        self.assertNotIn("bounded_json_preview", voice)
        runtime = (ROOT / "magician/src/magician_v2/tool_result_runtime.rs").read_text(encoding="utf-8")
        self.assertIn('"raw_result_included": false', runtime)


if __name__ == "__main__":
    unittest.main()

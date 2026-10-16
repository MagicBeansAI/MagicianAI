#!/usr/bin/env python3
"""Provider-free regression tests for eval-runtime-performance-live.py."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("eval-runtime-performance-live.py")
SPEC = importlib.util.spec_from_file_location("runtime_performance_live_eval", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class RuntimePerformanceLiveEvalTests(unittest.TestCase):
    def test_percentile_uses_observed_nearest_rank(self) -> None:
        self.assertEqual(MODULE.percentile([10, 20, 30, 40], 0.95), 40)
        self.assertEqual(MODULE.percentile([40, 10, 30, 20], 0.50), 20)
        self.assertIsNone(MODULE.percentile([], 0.95))

    def test_bounded_usage_is_iterative_and_does_not_follow_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            nested = root
            for index in range(80):
                nested = nested / f"d{index}"
                nested.mkdir()
            payload = nested / "payload.bin"
            payload.write_bytes(b"x" * 4096)
            (root / "loop").symlink_to(root, target_is_directory=True)
            usage = MODULE.bounded_path_usage(root, 200)
            self.assertFalse(usage.truncated)
            self.assertEqual(usage.files, 1)
            self.assertGreaterEqual(usage.logical_bytes, 4096)

    def test_bounded_usage_reports_truncation_instead_of_false_complete(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index in range(10):
                (root / f"{index}.txt").write_text("value", encoding="utf-8")
            usage = MODULE.bounded_path_usage(root, 3)
            self.assertTrue(usage.truncated)
            self.assertEqual(usage.entries_visited, 3)

    def test_storage_delta_keeps_logical_and_allocated_bytes_distinct(self) -> None:
        before = {
            "db": {
                "logical_bytes": 100,
                "allocated_bytes": 4096,
                "truncated": False,
                "errors": [],
            }
        }
        after = {
            "db": {
                "logical_bytes": 300,
                "allocated_bytes": 8192,
                "truncated": False,
                "errors": [],
            }
        }
        delta = MODULE.storage_delta(before, after)
        self.assertEqual(delta["logical_bytes"], 200)
        self.assertEqual(delta["allocated_bytes"], 4096)
        self.assertTrue(delta["conclusive"])

    def test_report_reader_rejects_oversized_input_before_json_decode(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            path.write_bytes(b"{" + b"x" * 64)
            with patch.object(MODULE, "MAX_REPORT_BYTES", 32):
                with self.assertRaisesRegex(MODULE.EvalFailure, "limit is 32"):
                    MODULE.load_json(path)

    def test_process_sampler_detects_disappearance_and_peak_growth(self) -> None:
        readings = iter([100, 140, None, 110])
        with patch.object(MODULE, "process_rss_bytes", side_effect=lambda _pid: next(readings)):
            sampler = MODULE.ProcessSampler(42, 60)
            sampler.capture()
            sampler.capture()
            sampler.capture()
            summary = sampler.stop()
        self.assertEqual(summary.start_rss_bytes, 100)
        self.assertEqual(summary.peak_rss_bytes, 140)
        self.assertEqual(summary.peak_growth_bytes, 40)
        self.assertTrue(summary.process_disappeared)

    def test_crash_scan_only_reads_the_new_bounded_log_segment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "magician.log"
            path.write_text("old stack overflow\n", encoding="utf-8")
            offset = path.stat().st_size
            with path.open("a", encoding="utf-8") as handle:
                handle.write("healthy\n")
            text, truncated = MODULE.read_log_segment(path, offset, 1024)
            self.assertFalse(truncated)
            self.assertEqual(MODULE.crash_markers(text), [])
            with path.open("a", encoding="utf-8") as handle:
                handle.write("thread has overflowed its stack\n")
            text, _ = MODULE.read_log_segment(path, offset, 1024)
            self.assertIn("has overflowed its stack", MODULE.crash_markers(text))

    def test_log_rotation_reads_the_new_file_from_its_beginning(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "magician.log"
            path.write_text("healthy old generation\n", encoding="utf-8")
            metadata = path.stat()
            identity = (metadata.st_dev, metadata.st_ino)
            offset = metadata.st_size
            path.rename(root / "magician.log.1")
            path.write_text("fatal runtime error: stack overflow\n", encoding="utf-8")
            text, truncated = MODULE.read_log_segment(path, offset, 1024, identity)
            self.assertFalse(truncated)
            self.assertIn("stack overflow", MODULE.crash_markers(text))

    def test_web_and_hitl_source_reports_publish_comparable_metrics(self) -> None:
        web = MODULE.extract_source_report_metrics(
            "web-researcher",
            {
                "passed": True,
                "summary": {
                    "input_tokens": 2000,
                    "answer_ready_p95_ms": 900,
                    "llm_calls": 3,
                },
                "cases": [
                    {"case_id": "d", "mode": "direct", "answer_ready_ms": 700},
                    {"case_id": "x", "mode": "delegated", "answer_ready_ms": 900},
                ],
            },
        )
        self.assertEqual(web["direct_answer_ready_ms"], 700)
        self.assertEqual(web["delegated_answer_ready_ms"], 900)
        hitl = MODULE.extract_source_report_metrics(
            "preplan-hitl",
            {
                "passed": True,
                "summary": {"resolved_hitl": 1},
                "cases": [
                    {
                        "duration_ms": 800,
                        "hitl": [{"attention_latency_ms": 12.5}],
                    }
                ],
            },
        )
        self.assertEqual(hitl["attention_visible_p95_ms"], 12.5)
        self.assertEqual(hitl["resolved_hitl"], 1)

    def test_missing_baseline_metric_is_inconclusive_not_a_false_pass(self) -> None:
        args = MODULE.parse_args(["--self-test"])
        process = MODULE.ProcessSummary(42, 3, 3, 100, 120, 105, 20, 5, False)
        gates = MODULE.build_gates(
            [],
            process,
            {"conclusive": True, "allocated_bytes": 0},
            {"available": True},
            True,
            False,
            [],
            {},
            {"scenario.web-researcher.input_tokens": 1000},
            args,
        )
        gate = next(gate for gate in gates if gate.metric == "scenario.web-researcher.input_tokens")
        self.assertEqual(gate.status, "inconclusive")
        self.assertEqual(MODULE.overall_status(gates), "inconclusive")

    def test_context_regression_uses_context_headroom_not_latency_headroom(self) -> None:
        args = MODULE.parse_args(["--self-test"])
        metric = "scenario.web-researcher.input_tokens"
        self.assertEqual(MODULE.regression_limit(metric, 10_000, args), 12_000)

    def test_context_growth_exempts_explicit_rebootstrap_but_not_continuation(self) -> None:
        metrics = MODULE.llm_context_metrics(
            [
                {
                    "timestamp_ms": 1,
                    "execution_id": "exec-1",
                    "operation": "agentic_decision",
                    "input_tokens": 1000,
                    "prompt_projection_mode": "bootstrap",
                },
                {
                    "timestamp_ms": 2,
                    "execution_id": "exec-1",
                    "operation": "agentic_decision",
                    "input_tokens": 1125,
                    "prompt_projection_mode": "continuation",
                },
                {
                    "timestamp_ms": 3,
                    "execution_id": "exec-1",
                    "operation": "agentic_decision",
                    "input_tokens": 5000,
                    "prompt_projection_mode": "rebootstrap",
                },
                {
                    "timestamp_ms": 4,
                    "execution_id": "exec-1",
                    "operation": "agentic_decision",
                    "input_tokens": 5100,
                    "prompt_projection_mode": "continuation",
                },
            ]
        )
        self.assertEqual(metrics["context_continuation_growth_tokens_max"], 125)
        self.assertEqual(metrics["context_input_tokens_max"], 5100)

    def test_baseline_regression_fails_above_metric_specific_limit(self) -> None:
        args = MODULE.parse_args(["--self-test"])
        process = MODULE.ProcessSummary(42, 3, 3, 100, 120, 105, 20, 5, False)
        metric = "scenario.web-researcher.input_tokens"
        gates = MODULE.build_gates(
            [],
            process,
            {"conclusive": True, "allocated_bytes": 0},
            {"available": True},
            True,
            False,
            [],
            {metric: 12_001},
            {metric: 10_000},
            args,
        )
        gate = next(gate for gate in gates if gate.metric == metric)
        self.assertEqual(gate.status, "fail")
        self.assertEqual(MODULE.overall_status(gates), "fail")

    def test_baseline_capture_is_create_once_and_retains_first_metrics(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "baseline" / "report.json"
            self.assertTrue(MODULE.capture_baseline(baseline, {"suite.peak_rss_mib": 100.0}))
            self.assertFalse(MODULE.capture_baseline(baseline, {"suite.peak_rss_mib": 999.0}))
            self.assertEqual(
                MODULE.baseline_metrics(baseline),
                {"suite.peak_rss_mib": 100.0},
            )

    def test_existing_baseline_without_comparable_metrics_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "baseline.json"
            baseline.write_text('{"metrics":{"llm_calls":3}}', encoding="utf-8")
            with self.assertRaisesRegex(MODULE.EvalFailure, "no comparable performance metrics"):
                MODULE.baseline_metrics(baseline)

    def test_failed_or_inconclusive_run_cannot_promote_itself_to_baseline(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "baseline.json"
            for status in ("fail", "inconclusive"):
                state, gate = MODULE.maybe_capture_missing_baseline(
                    baseline,
                    {"suite.peak_rss_mib": 100.0},
                    status,
                    True,
                )
                self.assertEqual(state, "not_captured")
                self.assertIsNotNone(gate)
                self.assertFalse(baseline.exists())

    def test_self_test_captures_then_compares_stable_baseline(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline = root / "baseline" / "report.json"
            first_output = root / "first"
            second_output = root / "second"
            common = [
                "--self-test",
                "--baseline",
                str(baseline),
                "--capture-baseline-if-missing",
            ]
            self.assertEqual(MODULE.main([*common, "--output-dir", str(first_output)]), 0)
            first = json.loads((first_output / "report.json").read_text(encoding="utf-8"))
            self.assertEqual(first["baseline_state"], "captured")
            self.assertTrue(baseline.is_file())

            self.assertEqual(MODULE.main([*common, "--output-dir", str(second_output)]), 0)
            second = json.loads((second_output / "report.json").read_text(encoding="utf-8"))
            self.assertEqual(second["baseline_state"], "compared")
            self.assertTrue(
                any(gate["name"] == "baseline.no_regression" for gate in second["gates"])
            )

    def test_command_timeout_returns_failure_and_does_not_wait_for_child(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            started = time.monotonic()
            result = MODULE.run_command_scenario(
                "timeout-fixture",
                [sys.executable, "-c", "import time; time.sleep(30)"],
                root / "missing-report.json",
                root / "command.log",
                None,
                0.01,
                0.05,
            )
            self.assertEqual(result.status, "fail")
            self.assertIn("exceeded", result.error or "")
            self.assertLess(time.monotonic() - started, 2)

    def test_successful_command_cannot_reuse_a_stale_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = root / "report.json"
            report.write_text('{"passed": true}', encoding="utf-8")
            result = MODULE.run_command_scenario(
                "stale-fixture",
                [sys.executable, "-c", "pass"],
                report,
                root / "command.log",
                None,
                0.01,
                2,
            )
            self.assertEqual(result.status, "fail")
            self.assertIn("cannot read JSON report", result.error or "")
            self.assertFalse(report.exists())

    def test_self_test_writes_a_baseline_ready_html_and_json_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "report"
            status = MODULE.main(["--self-test", "--output-dir", str(output)])
            self.assertEqual(status, 0)
            payload = json.loads((output / "report.json").read_text(encoding="utf-8"))
            self.assertEqual(payload["status"], "pass")
            self.assertIn("suite.peak_rss_mib", payload["metrics"])
            self.assertTrue((output / "report.html").is_file())

    def test_cli_bounds_sampling_and_attention_fanout(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                MODULE.parse_args(["--sample-interval-seconds", "0.001"])
            with self.assertRaises(SystemExit):
                MODULE.parse_args(["--attention-concurrency", "33"])


if __name__ == "__main__":
    unittest.main()

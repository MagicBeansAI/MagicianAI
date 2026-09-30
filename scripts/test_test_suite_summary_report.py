#!/usr/bin/env python3

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("test_suite_summary_report.py")
SPEC = importlib.util.spec_from_file_location("test_suite_summary_report", MODULE_PATH)
REPORT = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules[SPEC.name] = REPORT
SPEC.loader.exec_module(REPORT)


class TestSuiteSummaryReportTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.child = self.root / "rust.html"
        self.child.write_text("rust report", encoding="utf-8")
        self.manifest = self.root / "manifest.json"

    def tearDown(self):
        self.temp_dir.cleanup()

    def suite(self, **overrides):
        values = {
            "name": "Rust",
            "state": "passed",
            "exit_code": 0,
            "target": "test-rust",
            "description": "Workspace tests",
            "report_kind": "html",
            "report_path": self.child,
        }
        values.update(overrides)
        return REPORT.SuiteResult(**values)

    def render(self, suites):
        return REPORT.render_report(
            suites,
            mode="standard",
            run_id="20260713-120000-42",
            started_at="2026-07-13 12:00:00 IST",
            duration_seconds=125,
            manifest_path=self.manifest,
        )

    def test_links_available_child_reports_and_shows_run_metadata(self):
        rendered = self.render([self.suite()])

        self.assertIn(self.child.resolve().as_uri(), rendered)
        self.assertIn("Open child report", rendered)
        self.assertIn("2m 5s", rendered)
        self.assertIn("20260713-120000-42", rendered)

    def test_failure_controls_overall_result_and_missing_report_message(self):
        rendered = self.render([
            self.suite(
                name="Unified UI",
                state="failed",
                exit_code=1,
                target="test-ui",
                report_path=None,
            )
        ])

        self.assertIn("Magician complete test summary · Failed", rendered)
        self.assertIn('<span class="overall failed">Failed</span>', rendered)
        self.assertIn("A child HTML report was not produced", rendered)
        self.assertIn("Exit 1", rendered)

    def test_status_only_suite_explains_why_it_has_no_link(self):
        rendered = self.render([
            self.suite(
                name="Desktop tray",
                report_kind="status",
                report_path=None,
            )
        ])

        self.assertIn("this suite has no dedicated HTML report", rendered)

    def test_live_eval_suite_links_aggregate_report(self):
        live_report = self.root / "live-evals.html"
        live_report.write_text("live eval report", encoding="utf-8")
        rendered = self.render([
            self.suite(
                name="Live LLM evals",
                target="test-live-evals",
                description="Optional live LLM evals",
                report_path=live_report,
            )
        ])

        self.assertIn("optional live LLM evals", rendered)
        self.assertIn("make test-live-evals", rendered)
        self.assertIn(live_report.resolve().as_uri(), rendered)

    def test_imports_individual_live_eval_reports_from_child_manifest(self):
        live_report = self.root / "task-state.html"
        live_report.write_text("task-state live report", encoding="utf-8")
        child_manifest = self.root / "live-manifest.json"
        child_manifest.write_text(json.dumps({
            "suites": [{
                "name": "Task-state schema",
                "state": "passed",
                "exit_code": 0,
                "target": "test-live-evals",
                "description": "Task-state semantic parity",
                "report_kind": "html",
                "report_path": str(live_report),
            }]
        }), encoding="utf-8")

        suites = REPORT.parse_suite_manifest(child_manifest, "Live eval · ")
        rendered = self.render(suites)

        self.assertEqual(suites[0].name, "Live eval · Task-state schema")
        self.assertIn(live_report.resolve().as_uri(), rendered)
        self.assertIn("Task-state semantic parity", rendered)

    def test_imports_tool_authorization_report_into_top_level_dashboard(self):
        authorization_report = self.root / "tool-authorization.html"
        authorization_report.write_text("authorization report", encoding="utf-8")
        child_manifest = self.root / "authorization-manifest.json"
        child_manifest.write_text(json.dumps({
            "suites": [{
                "name": "Tool visibility authorization",
                "state": "passed",
                "exit_code": 0,
                "target": "test-agent-tool-visibility-live-eval",
                "description": "Production snapshot baseline comparison",
                "report_kind": "html",
                "report_path": str(authorization_report),
            }]
        }), encoding="utf-8")

        suites = REPORT.parse_suite_manifest(child_manifest, "Live eval · ")
        rendered = self.render(suites)

        self.assertEqual(
            suites[0].name,
            "Live eval · Tool visibility authorization",
        )
        self.assertIn(authorization_report.resolve().as_uri(), rendered)
        self.assertIn("Production snapshot baseline comparison", rendered)

    def test_rejects_malformed_child_suite_manifest(self):
        child_manifest = self.root / "bad-manifest.json"
        child_manifest.write_text('{"overall":"passed"}', encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "no suites array"):
            REPORT.parse_suite_manifest(child_manifest)

    def test_manifest_records_skips_failures_and_report_availability(self):
        suites = [
            self.suite(),
            self.suite(name="iOS", state="skipped", target="test-ios", report_path=None),
            self.suite(name="UI", state="failed", exit_code=2, target="test-ui", report_path=None),
        ]
        REPORT.write_manifest(
            self.manifest,
            suites,
            mode="verbose",
            run_id="run-1",
            started_at="now",
            duration_seconds=3,
        )

        payload = json.loads(self.manifest.read_text(encoding="utf-8"))
        self.assertEqual(payload["overall"], "failed")
        self.assertTrue(payload["suites"][0]["report_available"])
        self.assertEqual(payload["suites"][1]["state"], "skipped")
        self.assertEqual(payload["suites"][2]["exit_code"], 2)

    def test_nonzero_exit_always_normalizes_to_failed(self):
        parsed = REPORT.parse_suite([
            "Rust", "passed", "101", "test-rust", "Tests", "html", "",
        ])

        self.assertEqual(parsed.state, "failed")


if __name__ == "__main__":
    unittest.main()

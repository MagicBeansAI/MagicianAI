#!/usr/bin/env python3

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("magios_test_report.py")
SPEC = importlib.util.spec_from_file_location("magios_test_report", MODULE_PATH)
REPORT = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(REPORT)


class MagiosTestReportTests(unittest.TestCase):
    def test_collects_cases_with_suite_bundle_failure_and_duration(self):
        tree = {"testNodes": [{"name": "Plan", "nodeType": "Test Plan", "children": [{
            "name": "MagiosTests", "nodeType": "Unit test bundle", "children": [{
                "name": "TodayTests", "nodeType": "Test Suite", "children": [{
                    "name": "testWorks()", "nodeType": "Test Case", "result": "Passed",
                    "durationInSeconds": 0.25,
                }, {
                    "name": "testFails()", "nodeType": "Test Case", "result": "Failed",
                    "children": [{"name": "expected true", "nodeType": "Failure Message"}],
                }],
            }],
        }]}]}

        cases = REPORT.collect_tests(tree)

        self.assertEqual([case["name"] for case in cases], ["testWorks()", "testFails()"])
        self.assertEqual(cases[0]["bundle"], "MagiosTests")
        self.assertEqual(cases[0]["suite"], "TodayTests")
        self.assertEqual(cases[1]["failures"], ["expected true"])

    def test_render_contains_summary_tests_coverage_and_escaped_failure(self):
        summary = {
            "result": "Failed", "totalTestCount": 2, "passedTests": 1, "failedTests": 1,
            "skippedTests": 0, "startTime": 10.0, "finishTime": 12.5,
            "testFailures": [{"testName": "testFails()", "failureText": "<bad>"}],
        }
        tree = {"testNodes": [{"name": "TodayTests", "nodeType": "Test Suite", "children": [
            {"name": "testWorks()", "nodeType": "Test Case", "result": "Passed", "durationInSeconds": 0.1},
            {"name": "testFails()", "nodeType": "Test Case", "result": "Failed", "durationInSeconds": 0.2},
        ]}]}
        coverage = {"targets": [{
            "name": "Magican.app", "lineCoverage": 0.8, "files": [{
                "name": "Today.swift", "path": "/tmp/Today.swift", "lineCoverage": 0.75,
                "coveredLines": 75, "executableLines": 100, "functions": [],
            }],
        }]}

        rendered = REPORT.render_report(summary, tree, coverage, Path("missing.xcresult"), 65, [])

        self.assertIn("Test & coverage report", rendered)
        self.assertIn("Magican.app", rendered)
        self.assertIn("Today.swift", rendered)
        self.assertIn("75.00%", rendered)
        self.assertIn("&lt;bad&gt;", rendered)
        self.assertNotIn("<bad>", rendered)

    def test_nonzero_xcode_status_overrides_a_partial_passed_summary(self):
        rendered = REPORT.render_report(
            {"result": "Passed", "totalTestCount": 1, "passedTests": 1},
            None,
            None,
            Path("missing.xcresult"),
            65,
            [],
        )

        self.assertIn("Magican iOS test report · Failed", rendered)
        self.assertIn('<span class="result failed">Failed</span>', rendered)
        self.assertIn("xcodebuild exit 65", rendered)

    def test_failed_case_overrides_zero_status_and_partial_passed_summary(self):
        tree = {
            "testNodes": [
                {
                    "name": "testFails()",
                    "nodeType": "Test Case",
                    "result": "Failed",
                }
            ]
        }
        rendered = REPORT.render_report(
            {"result": "Passed", "totalTestCount": 1, "passedTests": 1},
            tree,
            None,
            Path("current.xcresult"),
            0,
            [],
        )

        self.assertIn("Magican iOS test report · Failed", rendered)
        self.assertIn('<span class="result failed">Failed</span>', rendered)
        self.assertEqual(
            REPORT.report_exit_status(0, False, {"result": "Passed"}, tree),
            2,
        )

    def test_xcode_failure_remains_authoritative_when_extraction_also_fails(self):
        self.assertEqual(REPORT.report_exit_status(65, True, None, None), 0)

    def test_zero_status_without_current_summary_is_not_presented_as_passed(self):
        rendered = REPORT.render_report(
            None,
            None,
            None,
            Path("missing.xcresult"),
            0,
            ["current-run result bundle unavailable"],
            run_id="run-123",
        )

        self.assertIn("Magican iOS test report · Unknown", rendered)
        self.assertIn('<span class="result unknown">Unknown</span>', rendered)
        self.assertIn("run run-123", rendered)
        self.assertNotIn('<span class="result passed">Passed</span>', rendered)

    def test_bundle_freshness_rejects_pre_run_artifact(self):
        with tempfile.TemporaryDirectory(prefix="magios-report-freshness-") as temp:
            bundle = Path(temp) / "result.xcresult"
            bundle.mkdir()
            os.utime(bundle, (100.0, 100.0))

            self.assertFalse(REPORT.bundle_is_fresh(bundle, 200.0))
            self.assertTrue(REPORT.bundle_is_fresh(bundle, 100.0))

    def test_stale_bundle_is_not_linked_as_current(self):
        with tempfile.TemporaryDirectory(prefix="magios-report-link-") as temp:
            bundle = Path(temp) / "old.xcresult"
            bundle.mkdir()
            rendered = REPORT.render_report(
                None,
                None,
                None,
                bundle,
                134,
                ["stale bundle refused"],
                bundle_is_current=False,
            )

            self.assertNotIn("raw xcresult", rendered)


if __name__ == "__main__":
    unittest.main()

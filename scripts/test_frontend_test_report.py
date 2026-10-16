#!/usr/bin/env python3

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("frontend_test_report.py")
SPEC = importlib.util.spec_from_file_location("frontend_test_report", MODULE_PATH)
REPORT = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(REPORT)


class FrontendTestReportTests(unittest.TestCase):
    def setUp(self):
        self.results = {
            "success": False,
            "numTotalTests": 2,
            "numPassedTests": 1,
            "numFailedTests": 1,
            "numPendingTests": 0,
            "numTodoTests": 0,
            "startTime": 1000,
            "testResults": [{
                "name": "/repo/ui/unified-ui/src/lib/example.test.ts",
                "endTime": 3500,
                "assertionResults": [{
                    "fullName": "example passes",
                    "status": "passed",
                    "duration": 10,
                    "failureMessages": [],
                }, {
                    "fullName": "example <fails>",
                    "status": "failed",
                    "duration": 20,
                    "failureMessages": ["expected <true>"],
                }],
            }],
        }
        self.coverage = {
            "total": {
                "lines": {"total": 10, "covered": 8, "pct": 80},
                "statements": {"total": 10, "covered": 8, "pct": 80},
                "functions": {"total": 4, "covered": 3, "pct": 75},
                "branches": {"total": 6, "covered": 3, "pct": 50},
            },
            "/repo/ui/unified-ui/src/lib/example.ts": {
                "lines": {"total": 10, "covered": 8, "pct": 80},
                "statements": {"total": 10, "covered": 8, "pct": 80},
                "functions": {"total": 4, "covered": 3, "pct": 75},
                "branches": {"total": 6, "covered": 3, "pct": 50},
            },
        }

    def test_renders_test_details_failures_and_coverage(self):
        rendered = REPORT.render_report(
            self.results,
            self.coverage,
            Path("missing-results.json"),
            Path("missing-coverage.html"),
            1,
            [],
        )

        self.assertIn("Magican frontend test report · Failed", rendered)
        self.assertIn("src/lib/example.test.ts", rendered)
        self.assertIn("example &lt;fails&gt;", rendered)
        self.assertIn("expected &lt;true&gt;", rendered)
        self.assertIn("src/lib/example.ts", rendered)
        self.assertIn("80.00%", rendered)

    def test_nonzero_status_overrides_successful_vitest_json(self):
        self.results["success"] = True
        self.results["numFailedTests"] = 0

        rendered = REPORT.render_report(
            self.results,
            self.coverage,
            Path("missing-results.json"),
            Path("missing-coverage.html"),
            65,
            [],
        )

        self.assertIn('<span class="result failed">Failed</span>', rendered)
        self.assertIn("test command exit 65", rendered)

    def test_missing_artifacts_produce_a_useful_failed_report(self):
        rendered = REPORT.render_report(
            None,
            None,
            Path("missing-results.json"),
            Path("missing-coverage.html"),
            1,
            ["Vitest results were not created"],
        )

        self.assertIn("Report warnings", rendered)
        self.assertIn("Vitest results were not created", rendered)
        self.assertIn("Coverage was not available", rendered)


if __name__ == "__main__":
    unittest.main()

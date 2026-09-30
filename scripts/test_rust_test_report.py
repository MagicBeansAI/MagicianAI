#!/usr/bin/env python3

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("rust_test_report.py")
SPEC = importlib.util.spec_from_file_location("rust_test_report", MODULE_PATH)
REPORT = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules[SPEC.name] = REPORT
SPEC.loader.exec_module(REPORT)


class RustTestReportTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.junit = self.root / "nextest.xml"
        self.junit.write_text("""<?xml version="1.0"?>
<testsuites>
  <testsuite name="magician::lib">
    <testcase classname="magician::today" name="loads local day" time="0.125" />
    <testcase classname="magician::today" name="escapes &lt;failure&gt;" time="0.5">
      <failure message="assertion failed">left: &lt;today&gt;</failure>
      <system-err>diagnostic &lt;output&gt;</system-err>
    </testcase>
    <testcase classname="magician::today" name="future case"><skipped /></testcase>
  </testsuite>
</testsuites>
""", encoding="utf-8")
        self.doctest = self.root / "doctests.log"
        self.doctest.write_text("""Doc-tests magicllm

running 1 test
test magicllm/src/lib.rs - example (line 12) ... ok
""", encoding="utf-8")
        self.nextest_log = self.root / "nextest.log"
        self.nextest_log.write_text(
            "Summary [ 1.000s] 3 tests run: 3 passed, 2 skipped\n",
            encoding="utf-8",
        )
        self.coverage = {
            "data": [{
                "files": [{
                    "filename": "/repo/magician/magician/src/today.rs",
                    "summary": {
                        "lines": {"count": 10, "covered": 8, "percent": 80},
                        "functions": {"count": 4, "covered": 3, "percent": 75},
                        "branches": {"count": 6, "covered": 3, "percent": 50},
                        "regions": {"count": 12, "covered": 9, "percent": 75},
                    },
                }],
                "totals": {
                    "lines": {"count": 10, "covered": 8, "percent": 80},
                    "functions": {"count": 4, "covered": 3, "percent": 75},
                    "branches": {"count": 6, "covered": 3, "percent": 50},
                    "regions": {"count": 12, "covered": 9, "percent": 75},
                },
            }],
        }

    def tearDown(self):
        self.temp_dir.cleanup()

    def test_parses_nextest_and_doctest_cases(self):
        suites, error = REPORT.parse_junit(self.junit)
        doctests, doctest_error = REPORT.parse_doctests(self.doctest)

        self.assertIsNone(error)
        self.assertIsNone(doctest_error)
        self.assertEqual([case.status for case in suites[0].cases], ["passed", "failed", "skipped"])
        self.assertEqual(doctests[0].cases[0].status, "passed")

    def test_parses_nextest_discovery_skips_as_an_aggregate(self):
        suites, error = REPORT.parse_nextest_skipped(self.nextest_log)

        self.assertIsNone(error)
        self.assertEqual(suites[0].cases[0].status, "skipped")
        self.assertEqual(suites[0].cases[0].count, 2)

    def test_renders_failures_suite_details_and_coverage(self):
        suites, _ = REPORT.parse_junit(self.junit)
        doctests, _ = REPORT.parse_doctests(self.doctest)
        rendered = REPORT.render_report(
            suites + doctests,
            self.coverage,
            self.junit,
            self.nextest_log,
            self.doctest,
            self.root / "coverage.json",
            self.root / "coverage" / "index.html",
            1,
            0,
            0,
            [],
        )

        self.assertIn("Magican Rust test report · Failed", rendered)
        self.assertIn("magician::lib", rendered)
        self.assertIn("escapes &lt;failure&gt;", rendered)
        self.assertIn("left: &lt;today&gt;", rendered)
        self.assertIn("doctest · magicllm", rendered)
        self.assertIn("magician/src/today.rs", rendered)
        self.assertIn("80.00%", rendered)

    def test_command_status_overrides_passing_cases(self):
        suites = [REPORT.TestSuite("suite", [REPORT.TestCase("suite", "passes", "passed")])]
        rendered = REPORT.render_report(
            suites,
            self.coverage,
            self.junit,
            self.nextest_log,
            self.doctest,
            self.root / "coverage.json",
            self.root / "coverage" / "index.html",
            0,
            101,
            0,
            [],
        )

        self.assertIn('<span class="result failed">Failed</span>', rendered)
        self.assertIn("doctest exit 101", rendered)

    def test_zero_count_coverage_metric_is_unavailable_not_zero_percent(self):
        self.assertEqual(REPORT.metric_text({"branches": {"count": 0, "covered": 0, "percent": 0}}, "branches"), "N/A")

    def test_missing_artifacts_produce_useful_warnings(self):
        suites, junit_error = REPORT.parse_junit(self.root / "missing.xml")
        doctests, doctest_error = REPORT.parse_doctests(self.root / "missing.log")
        rendered = REPORT.render_report(
            suites + doctests,
            None,
            self.root / "missing.xml",
            self.root / "missing-nextest.log",
            self.root / "missing.log",
            self.root / "missing.json",
            self.root / "missing-coverage.html",
            1,
            1,
            1,
            [junit_error, doctest_error],
        )

        self.assertIn("Report warnings", rendered)
        self.assertIn("JUnit results were not created", rendered)
        self.assertIn("Coverage was not available", rendered)


if __name__ == "__main__":
    unittest.main()

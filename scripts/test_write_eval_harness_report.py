#!/usr/bin/env python3
"""Provider-free regressions for write_eval_harness_report.py."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("write_eval_harness_report.py")
SPEC = importlib.util.spec_from_file_location("write_eval_harness_report", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class WriteEvalHarnessReportTests(unittest.TestCase):
    def test_output_tail_keeps_short_text(self) -> None:
        self.assertEqual(MODULE.output_tail("a\nb\n"), "a\nb")

    def test_output_tail_omits_prefix_on_long_text(self) -> None:
        text = "\n".join(str(index) for index in range(250))
        tail = MODULE.output_tail(text, limit=5)
        self.assertIn("245 earlier lines omitted", tail)
        self.assertTrue(tail.endswith("249"))
        self.assertNotIn("\n0\n", f"\n{tail}\n")

    def test_success_and_failure_both_write_html_and_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ok_dir = root / "ok"
            fail_dir = root / "fail"
            self.assertEqual(
                MODULE.run_and_report(
                    title="ok harness",
                    output_dir=ok_dir,
                    command=[sys.executable, "-c", "print('hello-eval')"],
                ),
                0,
            )
            self.assertEqual(
                MODULE.run_and_report(
                    title="fail harness",
                    output_dir=fail_dir,
                    command=[sys.executable, "-c", "raise SystemExit(4)"],
                ),
                4,
            )
            ok = json.loads((ok_dir / "report.json").read_text(encoding="utf-8"))
            fail = json.loads((fail_dir / "report.json").read_text(encoding="utf-8"))
            self.assertEqual(ok["status"], "passed")
            self.assertEqual(ok["exit_code"], 0)
            self.assertIn("hello-eval", (ok_dir / "output.txt").read_text(encoding="utf-8"))
            self.assertIn("ok harness", (ok_dir / "report.html").read_text(encoding="utf-8"))
            self.assertEqual(fail["status"], "failed")
            self.assertEqual(fail["exit_code"], 4)
            self.assertTrue((fail_dir / "report.html").is_file())

    def test_self_test_flag_passes(self) -> None:
        self.assertEqual(MODULE.main(["--self-test"]), 0)

    def test_missing_command_is_usage_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(MODULE.main(["--title", "x", "--output-dir", tmp]), 2)


if __name__ == "__main__":
    unittest.main()

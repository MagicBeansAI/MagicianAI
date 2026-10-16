from __future__ import annotations

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/eval-capability-recall.py"
SPEC = importlib.util.spec_from_file_location("capability_recall_eval", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def valid_report() -> dict:
    cases = [
        {
            "kind": "agent",
            "query": f"q{index}",
            "expected": ["web-researcher"],
            "hits": ["web-researcher"],
            "golden": True,
            "passed": True,
            "note": "golden",
        }
        for index in range(20)
    ]
    return {
        "schema_version": 1,
        "generated_by": MODULE.GENERATED_BY,
        "gate": "PASS",
        "golden_passed": 20,
        "golden_total": 20,
        "auto_passed": 0,
        "auto_total": 0,
        "agent_count": 30,
        "tool_leaf_count": 100,
        "cases": cases,
    }


class CapabilityRecallEvalTest(unittest.TestCase):
    def test_valid_production_report_passes(self) -> None:
        self.assertEqual(MODULE.validate_report(valid_report()), [])

    def test_golden_miss_fails(self) -> None:
        report = valid_report()
        report["cases"][0]["passed"] = False
        report["cases"][0]["query"] = "web research"
        report["gate"] = "PASS"
        failures = MODULE.validate_report(report)
        self.assertTrue(any("golden misses" in item for item in failures))

    def test_report_writer_emits_json_and_html(self) -> None:
        report = valid_report()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "run"
            report_root = Path(directory) / "reports"
            with patch.dict(os.environ, {"CAPABILITY_RECALL_EVAL_REPORT_DIR": str(report_root)}):
                json_path, html_path = MODULE.write_report(ROOT, report, [], output)
            self.assertEqual(json.loads(json_path.read_text())["gate"], "PASS")
            body = html_path.read_text()
            self.assertIn("Tool and agent recall eval", body)
            self.assertTrue((report_root / "latest.html").is_file())


if __name__ == "__main__":
    unittest.main()

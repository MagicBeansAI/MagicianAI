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
MODULE_PATH = ROOT / "scripts/eval-agent-surface-runtime-cache.py"
SPEC = importlib.util.spec_from_file_location("agent_surface_runtime_cache_eval", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def valid_report() -> dict:
    scenarios = [
        {"name": name, "passed": True, "assertions": index + 1, "details": {}}
        for index, name in enumerate(sorted(MODULE.REQUIRED_SCENARIOS))
    ]
    return {
        "schema_version": 1,
        "generated_by": MODULE.GENERATED_BY,
        "gate": "PASS",
        "scenario_count": len(scenarios),
        "passed_count": len(scenarios),
        "scenarios": scenarios,
    }


class AgentSurfaceRuntimeCacheEvalTest(unittest.TestCase):
    def test_valid_production_report_passes(self) -> None:
        self.assertEqual(MODULE.validate_report(valid_report()), [])

    def test_synthetic_or_failed_report_is_rejected(self) -> None:
        report = valid_report()
        report["generated_by"] = "synthetic"
        report["scenarios"][0]["passed"] = False
        failures = MODULE.validate_report(report)
        self.assertTrue(any("production Rust evaluator" in item for item in failures))
        self.assertTrue(any("scenario failed" in item for item in failures))

    def test_report_writer_emits_json_and_clickable_html(self) -> None:
        report = valid_report()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "run"
            report_root = Path(directory) / "reports"
            with patch.dict(
                os.environ,
                {"AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR": str(report_root)},
            ):
                json_path, html_path = MODULE.write_report(ROOT, report, [], output)
            self.assertEqual(json.loads(json_path.read_text())["gate"], "PASS")
            body = html_path.read_text()
            self.assertIn("Agent surface runtime cache", body)
            self.assertIn("surface_working_sets_are_isolated_and_revision_safe", body)
            self.assertIn("production Rust evaluator", body)
            self.assertEqual(json.loads((report_root / "latest.json").read_text())["gate"], "PASS")
            self.assertTrue((report_root / "latest.html").is_file())


if __name__ == "__main__":
    unittest.main()

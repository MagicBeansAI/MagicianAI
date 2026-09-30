"""Focused checks that the report cannot turn missing evidence into acceptance."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("memory_eval",Path(__file__).with_name("eval-memory-connections.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def row(kind, passed=True, provider=True):
    return {"partition":"heldout","repeat":0,"id":kind,"expected":{"kind":kind},"automated_passed":passed,
            "checks":{"provider_ok":provider,"delivery_ok":True,"no_unrequested_memory_write":True,"cross_scope_clear":True}}


class ReportAcceptance(unittest.TestCase):
    def test_missing_cases_or_provider_failure_cannot_pass(self):
        good={"cases":[row("silent"),row("connection")],"expected_cases":2}
        self.assertTrue(module.summarize(good)["automated_gates_passed"])
        self.assertFalse(module.summarize({**good,"expected_cases":3})["automated_gates_passed"])
        self.assertFalse(module.summarize({**good,"cases":[row("silent",provider=False),row("connection")]})["automated_gates_passed"])

    def test_always_silent_fails_positive_gate(self):
        report={"cases":[row("silent"),row("connection",False)],"expected_cases":2}
        self.assertFalse(module.summarize(report)["automated_gates_passed"])

    def test_raw_model_markup_is_escaped(self):
        report={"cases":[{**row("silent"),"observations":[{"raw_response":"<script>alert(1)</script>"}]}]}
        rendered=module.render(report)
        self.assertNotIn("<script>alert(1)</script>",rendered)
        self.assertIn("&lt;script&gt;",rendered)


if __name__ == "__main__":
    unittest.main()

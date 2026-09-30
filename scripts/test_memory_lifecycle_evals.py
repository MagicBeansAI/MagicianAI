"""Focused wrapper contracts: no providers, credentials or running services."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from memory_lifecycle_report import summarize, render

SPEC = importlib.util.spec_from_file_location("lifecycle_lane", Path(__file__).with_name("eval-memory-lifecycle.py"))
lane = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lane)


class LifecycleLaneTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = self.root / "owner/magician-config.yaml"
        self.config.parent.mkdir()
        self.config.write_text("llm:\n  router:\n    profiles:\n      alpha: {provider: openai, model: candidate-a}\n      beta: {provider: anthropic, model: candidate-b}\n    operation_mapping:\n"+
                               "".join(f"      {op}: alpha\n" for op in lane.OPERATIONS))
        (self.config.parent / "system").mkdir()
        self.overrides = self.config.parent / "system/llm_routing_overrides.json"
        self.overrides.write_text(json.dumps({"memory_lifecycle_review":"beta", "unrelated":"alpha"}))
        self.suite = self.root / "fixtures/cases.json"
        self.suite.parent.mkdir()
        self.suite.write_text(json.dumps([{"id":"fixture", "partition":"validation"}]))
        for name in lane.FIXTURES[1:]: self.suite.with_name(name).write_text("{}")
        self.binary = self.root / "eval-binary"
        self.binary.write_text("fake binary bytes")

    def arguments(self, run_id="first", profiles="alpha,beta"):
        return ["--skip-build", "--config",str(self.config),"--suite",str(self.suite),
                "--binary",str(self.binary),"--output-dir",str(self.root/"reports"),
                "--run-id",run_id,"--profiles",profiles,"--repeats","1"]

    def test_snapshots_preserve_config_and_override_only_fixture_operations(self):
        original = self.config.read_bytes(), self.overrides.read_bytes()
        snapshots = lane.snapshot_configs(self.config,["alpha","beta"],self.root/"private")
        for expected, config in snapshots:
            overrides = json.loads((config.parent/"system/llm_routing_overrides.json").read_text())
            self.assertEqual({overrides[op] for op in lane.OPERATIONS},{expected})
            self.assertEqual(overrides["unrelated"],"alpha")
            self.assertEqual(config.stat().st_mode & 0o777,0o600)
            self.assertEqual(config.read_bytes(),original[0])
        self.assertEqual((self.config.read_bytes(),self.overrides.read_bytes()), original)
        configured = lane.snapshot_configs(self.config,[],self.root/"default")[0][1]
        self.assertEqual(json.loads((configured.parent/"system/llm_routing_overrides.json").read_text()),json.loads(original[1]))

    def test_unknown_profile_refuses_entire_comparison_before_any_call(self):
        with patch.object(lane.subprocess,"run") as run:
            self.assertEqual(lane.main(self.arguments(profiles="alpha,missing")),1)
            run.assert_not_called()
        report=json.loads((self.root/"reports/runs/first/report.json").read_text())
        self.assertEqual(report["status"],"failed")
        self.assertIn("Unknown configured profile",report["error"])

    def test_failed_then_passing_profiles_both_retained_and_reruns_are_immutable(self):
        def fake(command, **_):
            output=Path(command[command.index("--output-dir")+1]);output.mkdir()
            failed=output.name=="profile-0"
            cases=[{"id":str(i),"repeat":0,"passed":not failed,"events":[]} for i in range(3)]
            (output/"report.json").write_text(json.dumps({"cases":cases}))
            return subprocess.CompletedProcess(command,1 if failed else 0,"fixture output","")
        with patch.object(lane.subprocess,"run",side_effect=fake):
            self.assertEqual(lane.main(self.arguments()),1)
            first=(self.root/"reports/runs/first/report.json").read_bytes()
            self.assertEqual(lane.main(self.arguments(run_id="second")),1)
            self.assertEqual((self.root/"reports/runs/first/report.json").read_bytes(),first)
            with self.assertRaises(FileExistsError): lane.main(self.arguments())
        report=json.loads(first)
        self.assertEqual([r["passed"] for r in report["runs"]],[False,True])
        self.assertTrue((self.root/"reports/runs/first/report.html").is_file())
        self.assertFalse((self.root/"reports/runs/first/magician-config.yaml").exists())

    def test_missing_evidence_cannot_be_a_pass_even_with_zero_exit(self):
        with patch.object(lane.subprocess,"run",return_value=subprocess.CompletedProcess([],0,"","")):
            self.assertEqual(lane.main(self.arguments(profiles="alpha")),1)
        report=json.loads((self.root/"reports/runs/first/report.json").read_text())
        self.assertEqual(report["runs"][0]["summary"]["journeys"],{"passed":0,"total":0})

    def test_timeout_retains_partial_evidence_and_continues_comparison(self):
        def fake(command, **_):
            output=Path(command[command.index("--output-dir")+1]);output.mkdir()
            (output/"report.json").write_text(json.dumps({"cases":[{"id":str(i),"repeat":0,"passed":True} for i in range(3)]}))
            if output.name == "profile-0":
                raise subprocess.TimeoutExpired(command, 1, output=b"partial output")
            return subprocess.CompletedProcess(command,0,"","")
        with patch.object(lane.subprocess,"run",side_effect=fake):
            self.assertEqual(lane.main(self.arguments()),1)
        report=json.loads((self.root/"reports/runs/first/report.json").read_text())
        self.assertEqual([r["passed"] for r in report["runs"]],[False,True])
        self.assertFalse(report["runs"][0]["complete"])
        self.assertIn("partial output",(self.root/"reports/runs/first/output.txt").read_text())

    def test_deterministic_lane_retains_exit_and_report(self):
        with patch.object(lane,"run_command",return_value=(7,"test failed\n",0.1)) as command:
            self.assertEqual(lane.main(self.arguments()+["--mode","harness"]),7)
        command.assert_called_once_with(["make","--no-print-directory","test-memory-lifecycle-unit"])
        output=self.root/"reports/runs/first"
        self.assertTrue((output/"report.html").is_file())
        self.assertEqual((output/"output.txt").read_text(),"test failed\n")

    def test_build_failure_still_writes_a_report_without_model_calls(self):
        args=self.arguments(profiles="alpha");args.remove("--skip-build")
        with patch.object(lane,"run_command",return_value=(7,"compile failed",0.1)), patch.object(lane.subprocess,"run") as run:
            self.assertEqual(lane.main(args),1)
            run.assert_not_called()
        self.assertIn("compile failed",(self.root/"reports/runs/first/output.txt").read_text())

    def test_error_sidecar_is_counted_once_and_missing_cost_is_unknown(self):
        raw=self.root/"raw";side=raw/"free-text-clarification-0";side.mkdir(parents=True)
        observation={"model":"candidate","provider":"provider","usage":{"input_tokens":9,"output_tokens":4},"router_estimated_cost_usd":None}
        (side/"failed-initial-review.json").write_text(json.dumps({"observations":[observation]}))
        report={"cases":[{"id":"free-text-clarification","repeat":0,"passed":False,"root":"/untrusted/path","clarification":{"error":"invalid JSON"}}]}
        summary=summarize(report,raw)
        self.assertEqual(summary["calls"],1)
        self.assertEqual(summary["input_tokens"],9)
        self.assertIsNone(summary["router_estimated_cost_usd"])
        self.assertFalse(summary["cost_complete"])
        report["cases"][0]["clarification"]["observations"]=[observation]
        self.assertEqual(summarize(report,raw)["calls"],1)

    def test_html_escapes_model_content_and_marks_partial_cost(self):
        report={"status":"failed","runs":[{"profile":"<script>bad</script>","duration_seconds":1,"evidence":"profile-0/report.json",
                "summary":{"journeys":{"passed":0,"total":1},"router_estimated_cost_usd":0.01,"cost_complete":False}}]}
        result=render(report)
        self.assertNotIn("<script>",result)
        self.assertIn("at least $0.010000",result)


if __name__ == "__main__": unittest.main()

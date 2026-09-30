#!/usr/bin/env python3
"""Deterministic regression tests for the recurring-monitor live harness."""

import importlib.util
import json
import os
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("eval-monitor-live.py")


def load_eval_module():
    spec = importlib.util.spec_from_file_location("eval_monitor_live_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class MonitorLiveEvalHarnessTests(unittest.TestCase):
    def test_defaults_to_an_isolated_non_user_scope(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            module = load_eval_module()
        self.assertEqual(module.BASE, "http://127.0.0.1:3002")
        self.assertEqual(module.PRINCIPAL, "live-eval")
        self.assertEqual(module.WORKSPACE, "monitoring")
        self.assertEqual(module.SCOPE, {})

    def test_sends_the_scoped_bearer_without_identity_headers(self):
        with mock.patch.dict(
            os.environ, {"MAGICIAN_BEARER_TOKEN": "mag_pat_scoped"}, clear=True
        ):
            module = load_eval_module()
        self.assertEqual(module.SCOPE, {"Authorization": "Bearer mag_pat_scoped"})

    def test_cleanup_cancels_timed_out_execution_and_physically_deletes(self):
        module = load_eval_module()
        calls = []
        delete_attempts = 0

        def fake_http(method, url, body=None, timeout=90):
            nonlocal delete_attempts
            calls.append((method, url, body))
            if method == "DELETE":
                delete_attempts += 1
                if delete_attempts == 1:
                    raise urllib.error.HTTPError(url, 409, "active", {}, None)
            return {}

        module.CLEANUP_TIMEOUT = 5
        module.POLL_SECS = 0
        with mock.patch.object(module, "http", side_effect=fake_http), mock.patch.object(
            module.time, "sleep", return_value=None
        ):
            error = module.cancel_and_physically_delete_monitor(
                "task_eval", ["exec_eval", "exec_eval"]
            )

        self.assertIsNone(error)
        self.assertEqual(
            [call for call in calls if call[0] == "POST"],
            [("POST", f"{module.API}/executions/exec_eval/cancel", {})],
        )
        self.assertEqual(delete_attempts, 2)
        self.assertTrue(
            all(
                url.endswith("/monitors/task_eval?remove_files=true")
                for method, url, _ in calls
                if method == "DELETE"
            )
        )

    def test_run_poll_exposes_execution_id_before_waiting(self):
        module = load_eval_module()
        seen = []

        def fake_http(method, url, body=None, timeout=90):
            if method == "POST":
                return {"execution": {"state": {"execution_id": "exec_early"}}}
            return {"items": [{"execution_id": "exec_early", "status": "baseline"}]}

        with mock.patch.object(module, "http", side_effect=fake_http):
            run, execution_id, _ = module.run_and_await(
                "task_eval", on_execution=seen.append
            )

        self.assertEqual(seen, ["exec_early"])
        self.assertEqual(execution_id, "exec_early")
        self.assertEqual(run["status"], "baseline")

    def test_partial_report_is_atomic_timestamped_and_html_escaped(self):
        module = load_eval_module()
        result = module.step_result("pricing<script>", 0, "baseline", "baseline")
        result["error"] = "failure <unsafe>"
        summary = module.build_summary(
            [result], "http://127.0.0.1:1234", completed=False, run_error="interrupted"
        )

        with tempfile.TemporaryDirectory() as directory:
            module.REPORT_DIR = directory
            module.write_report(summary, timestamped=True, stamp="20260724-120000")

            latest = json.loads(Path(directory, "latest.json").read_text())
            rendered = Path(directory, "latest.html").read_text()
            timestamped = Path(directory, "run-20260724-120000.json")

            self.assertFalse(latest["completed"])
            self.assertEqual(latest["run_error"], "interrupted")
            self.assertTrue(timestamped.is_file())
            self.assertIn("pricing&lt;script&gt;", rendered)
            self.assertIn("failure &lt;unsafe&gt;", rendered)
            self.assertFalse(list(Path(directory).glob("*.tmp-*")))


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Regression tests for the composed test runner's live-eval decision stage."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]
RUNNER = REPO_ROOT / "scripts" / "run-all-tests-with-report.sh"


class RunAllTestsLiveEvalDecisionTests(unittest.TestCase):
    def test_suite_runner_self_provisions_pinned_python_dependencies(self) -> None:
        makefile = (REPO_ROOT / "Makefile").read_text(encoding="utf-8")
        requirements = [
            line
            for line in (
                REPO_ROOT / "scripts" / "test-suite-runner-requirements.txt"
            ).read_text(encoding="utf-8").splitlines()
            if line and not line.startswith("#")
        ]

        self.assertIn(
            "test-suite-runner: setup-test-suite-runner-deps",
            makefile,
        )
        self.assertIn(
            "TEST_SUITE_RUNNER_VENV ?= $(CARGO_TARGET_DIR)/test-suite-runner-venv",
            makefile,
        )
        self.assertIn("sys.version_info >= (3, 10)", makefile)
        self.assertIn('"$(TEST_SUITE_RUNNER_PYTHON)" -m unittest', makefile)
        self.assertIn("import importlib.metadata as metadata", makefile)
        self.assertIn('"$(TEST_SUITE_RUNNER_PYTHON)" -m pip check', makefile)
        self.assertIn(
            "test-agent-tool-visibility-eval-harness: setup-test-suite-runner-deps",
            makefile,
        )
        for target in (
            "benchmark-media-offline-audio",
            "benchmark-media-vad",
            "benchmark-media-stt",
            "benchmark-media-tts",
            "benchmark-media-diarization",
            "test-media-offline-audio-eval",
        ):
            self.assertIn(f"{target}: setup-test-suite-runner-deps", makefile)
        self.assertIn(
            '"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py --self-test',
            makefile,
        )
        self.assertGreaterEqual(
            makefile.count(
                '"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py'
            ),
            6,
        )
        self.assertIn("import jsonschema, requests, websocket, yaml", makefile)
        self.assertGreaterEqual(
            makefile.count("LIVE_EVAL_PYTHON='$(TEST_SUITE_RUNNER_PYTHON)'"),
            2,
        )
        self.assertEqual(
            requirements,
            [
                "attrs==26.1.0",
                "certifi==2026.7.22",
                "charset-normalizer==3.5.1",
                "idna==3.19",
                "jsonschema==4.26.0",
                "jsonschema-specifications==2025.9.1",
                "PyYAML==6.0.3",
                "referencing==0.37.0",
                "requests==2.34.2",
                "rpds-py==0.30.0",
                "urllib3==2.7.0",
                "websocket-client==1.9.0",
            ],
        )

    def run_runner(
        self, *args: str, prompt_response: str | None = None
    ) -> tuple[subprocess.CompletedProcess[str], list[str]]:
        with tempfile.TemporaryDirectory(prefix="test-runner-live-evals-") as temp:
            temp_path = Path(temp)
            call_log = temp_path / "make-calls.log"
            fake_make = temp_path / "fake-make"
            fake_make.write_text(
                "#!/usr/bin/env bash\n"
                "printf '%s\\n' \"$*\" >> \"$TEST_CALL_LOG\"\n",
                encoding="utf-8",
            )
            fake_make.chmod(0o755)

            env = os.environ.copy()
            env.update(
                {
                    "MAKE_BIN": str(fake_make),
                    "TEST_CALL_LOG": str(call_log),
                    "TEST_SUMMARY_REPORT_DIR": str(temp_path / "coverage"),
                    "RUST_TEST_REPORT_DIR": str(temp_path / "rust"),
                    "UI_TEST_REPORT_DIR": str(temp_path / "frontend"),
                    "IOS_TEST_REPORT_DIR": str(temp_path / "ios"),
                    "LIVE_EVAL_REPORT_DIR": str(temp_path / "live"),
                    "AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR": str(
                        temp_path / "surface-runtime"
                    ),
                }
            )
            if prompt_response is None:
                env.pop("TEST_SUITE_LIVE_EVAL_PROMPT_RESPONSE", None)
            else:
                env["TEST_SUITE_LIVE_EVAL_PROMPT_RESPONSE"] = prompt_response

            completed = subprocess.run(
                [str(RUNNER), *args],
                cwd=REPO_ROOT,
                env=env,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                check=False,
            )
            calls = call_log.read_text(encoding="utf-8").splitlines() if call_log.exists() else []
            targets = [call.split()[-1] for call in calls]
            return completed, targets

    def test_prompt_yes_runs_live_evals_after_every_non_live_suite(self) -> None:
        completed, targets = self.run_runner(prompt_response="yes")

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("test-content-retrieval-eval-harness", targets)
        self.assertIn("test-magdroid", targets)
        self.assertEqual(targets[-1], "test-live-evals")
        self.assertLess(
            targets.index("test-content-retrieval-eval-harness"),
            targets.index("test-magdroid"),
        )
        self.assertLess(
            targets.index("test-magdroid"),
            targets.index("test-ios"),
        )
        self.assertLess(targets.index("test-ios"), targets.index("test-live-evals"))
        self.assertIn("All non-live test suites are complete", completed.stdout)

    def test_prompt_uppercase_yes_runs_live_evals(self) -> None:
        # Regression: a capital "Y"/"YES" answer must run the evals just like
        # the lowercase form (case-sensitivity previously rejected it).
        for answer in ("Y", "YES", "Yes"):
            with self.subTest(answer=answer):
                completed, targets = self.run_runner(prompt_response=answer)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                self.assertEqual(targets[-1], "test-live-evals")

    def test_prompt_uppercase_no_skips_live_evals(self) -> None:
        for answer in ("N", "NO", "No"):
            with self.subTest(answer=answer):
                completed, targets = self.run_runner(prompt_response=answer)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                self.assertNotIn("test-live-evals", targets)

    def test_explicit_false_skips_prompt_and_live_evals(self) -> None:
        completed, targets = self.run_runner("--live-evals=false")

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertNotIn("test-live-evals", targets)
        self.assertIn("test-content-retrieval-eval-harness", targets)
        self.assertIn("test-attention-historical-bootstrap-eval", targets)
        self.assertIn("test-magdroid", targets)
        self.assertEqual(targets[-1], "test-ios")

    def test_explicit_true_runs_live_evals_last(self) -> None:
        completed, targets = self.run_runner("--live-evals=true")

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(targets[-1], "test-live-evals")

    def test_projection_context_harness_runs_before_live_prompt_stage(self) -> None:
        completed, targets = self.run_runner("--live-evals=false")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("test-tool-result-projection-context-eval-harness", targets)
        self.assertIn("test-provider-replay-eval-harness", targets)
        self.assertIn("test-preplan-flow-eval-harness", targets)
        self.assertIn("test-web-researcher-eval-harness", targets)
        self.assertLess(
            targets.index("test-tool-result-projection-context-eval-harness"),
            targets.index("test-provider-replay-eval-harness"),
        )
        self.assertLess(
            targets.index("test-provider-replay-eval-harness"),
            targets.index("test-preplan-flow-eval-harness"),
        )
        self.assertLess(
            targets.index("test-preplan-flow-eval-harness"),
            targets.index("test-web-researcher-eval-harness"),
        )
        self.assertLess(
            targets.index("test-web-researcher-eval-harness"),
            targets.index("test-ios"),
        )

    def test_unspecified_noninteractive_run_skips_instead_of_hanging(self) -> None:
        completed, targets = self.run_runner()

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertNotIn("test-live-evals", targets)
        self.assertIn("No interactive terminal", completed.stdout)

    def test_invalid_explicit_value_fails_before_any_suite(self) -> None:
        completed, targets = self.run_runner("--live-evals=perhaps")

        self.assertEqual(completed.returncode, 2)
        self.assertEqual(targets, [])
        self.assertIn("Invalid live-eval choice", completed.stderr)

    def test_android_setup_check_and_test_are_wired_into_aggregate_targets(self) -> None:
        makefile = (REPO_ROOT / "Makefile").read_text(encoding="utf-8")
        runner = RUNNER.read_text(encoding="utf-8")
        check_all = next(
            line for line in makefile.splitlines() if line.startswith("check-all:")
        )

        self.assertIn("check-magdroid", check_all)
        self.assertIn("$(MAKE) setup-magdroid-build", makefile)
        self.assertIn(
            'android_target="${TEST_SUITE_ANDROID_TARGET:-test-magdroid}"',
            runner,
        )
        self.assertIn('--suite "Android (Magdroid)"', runner)


if __name__ == "__main__":
    unittest.main()

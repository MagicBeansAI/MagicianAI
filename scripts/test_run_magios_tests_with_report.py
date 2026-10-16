#!/usr/bin/env python3

import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("run_magios_tests_with_report.py")
SPEC = importlib.util.spec_from_file_location("run_magios_tests_with_report", MODULE_PATH)
RUNNER = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)


HOST_ABORT = """DVTFilePathFSEvents: Failed to start fs event stream
confstr(DARWIN_USER_CACHE_DIR) failed: Input/output error
"""


class MagiosTestRunnerTests(unittest.TestCase):
    @staticmethod
    def probe(results):
        def run(command):
            returncode, stdout, stderr = results.get(
                tuple(command), (127, "", "unconfigured probe")
            )
            return subprocess.CompletedProcess(command, returncode, stdout, stderr)

        return run

    def result(
        self,
        root: Path,
        number: int,
        status: int,
        stderr: str,
        *,
        create_bundle: bool = False,
        stdout: str = "",
    ):
        bundle = root / f"attempt-{number}.xcresult"
        if create_bundle:
            bundle.mkdir()
        return RUNNER.AttemptResult(
            number,
            status,
            bundle,
            root / f"attempt-{number}.stderr.log",
            stderr,
            stdout,
        )

    def test_exact_pretest_sigabrt_retries_once(self):
        with tempfile.TemporaryDirectory(prefix="magios-retry-") as temp:
            root = Path(temp)
            attempts = [
                self.result(root, 1, -signal.SIGABRT, HOST_ABORT),
                self.result(root, 2, 0, "", create_bundle=True),
            ]
            calls = []

            final, discarded = RUNNER.execute_with_bounded_retry(
                lambda number: calls.append(number) or attempts[number - 1], 1
            )

            self.assertEqual(calls, [1, 2])
            self.assertEqual(final.number, 2)
            self.assertEqual([result.number for result in discarded], [1])

    def test_persistent_host_abort_stops_after_one_retry(self):
        with tempfile.TemporaryDirectory(prefix="magios-bounded-retry-") as temp:
            root = Path(temp)
            attempts = [
                self.result(root, 1, -signal.SIGABRT, HOST_ABORT),
                self.result(root, 2, -signal.SIGABRT, HOST_ABORT),
            ]
            calls = []

            final, discarded = RUNNER.execute_with_bounded_retry(
                lambda number: calls.append(number) or attempts[number - 1], 1
            )

            self.assertEqual(calls, [1, 2])
            self.assertEqual(final.number, 2)
            self.assertTrue(RUNNER.is_retryable_pretest_abort(final))
            self.assertEqual(len(discarded), 1)

    def test_assertion_failure_is_never_retried(self):
        with tempfile.TemporaryDirectory(prefix="magios-no-test-retry-") as temp:
            root = Path(temp)
            calls = []
            failed = self.result(root, 1, 65, "Failing tests: testExpectedValue")

            final, discarded = RUNNER.execute_with_bounded_retry(
                lambda number: calls.append(number) or failed, 1
            )

            self.assertEqual(calls, [1])
            self.assertEqual(final.status, 65)
            self.assertEqual(discarded, [])

    def test_abort_with_xctest_activity_is_never_retried(self):
        with tempfile.TemporaryDirectory(prefix="magios-xctest-no-retry-") as temp:
            root = Path(temp)
            calls = []
            failed = self.result(
                root,
                1,
                -signal.SIGABRT,
                HOST_ABORT,
                stdout="Test Suite 'TodayTests' started\nFailing tests: testExpectedValue\n",
            )

            final, discarded = RUNNER.execute_with_bounded_retry(
                lambda number: calls.append(number) or failed, 1
            )

            self.assertEqual(calls, [1])
            self.assertEqual(final.status, -signal.SIGABRT)
            self.assertEqual(discarded, [])

    def test_abort_with_a_result_bundle_is_not_pretest_and_is_not_retried(self):
        with tempfile.TemporaryDirectory(prefix="magios-bundle-no-retry-") as temp:
            root = Path(temp)
            result = self.result(
                root,
                1,
                128 + signal.SIGABRT,
                HOST_ABORT,
                create_bundle=True,
            )

            self.assertFalse(RUNNER.is_retryable_pretest_abort(result))

    def test_abort_requires_both_host_failure_signatures(self):
        with tempfile.TemporaryDirectory(prefix="magios-signature-no-retry-") as temp:
            root = Path(temp)
            fsevents_only = self.result(
                root,
                1,
                -signal.SIGABRT,
                "DVTFilePathFSEvents: Failed to start fs event stream",
            )

            self.assertFalse(RUNNER.is_retryable_pretest_abort(fsevents_only))

    def test_stale_report_is_quarantined_and_cannot_be_republished(self):
        with tempfile.TemporaryDirectory(prefix="magios-report-publish-") as temp:
            root = Path(temp)
            latest = root / "latest.html"
            report = root / "reports" / "current.html"
            latest.write_text("old", encoding="utf-8")
            report.parent.mkdir()
            report.write_text("candidate", encoding="utf-8")
            os.utime(report, (100.0, 100.0))

            previous = RUNNER.quarantine_latest(latest)

            self.assertIsNotNone(previous)
            self.assertFalse(latest.exists())
            self.assertEqual(previous.read_text(encoding="utf-8"), "old")
            self.assertFalse(RUNNER.publish_report(report, latest, 200.0))
            self.assertFalse(latest.exists())

            os.utime(report, (200.0, 200.0))
            self.assertTrue(RUNNER.publish_report(report, latest, 200.0))
            self.assertEqual(latest.read_text(encoding="utf-8"), "candidate")

    def test_attempt_environment_uses_writable_isolated_temp_and_caches(self):
        with tempfile.TemporaryDirectory(prefix="magios-attempt-env-") as temp:
            root = Path(temp)
            attempt_root = root / "attempt"
            real_home = root / "real-home"
            simulator = real_home / "Library" / "Developer" / "CoreSimulator"
            simulator_devices = simulator / "Devices"
            simulator_cache = simulator / "Caches"
            user_data = real_home / "Library" / "Developer" / "Xcode" / "UserData"
            simulator_devices.mkdir(parents=True)
            simulator_cache.mkdir()
            user_data.mkdir(parents=True)

            with patch.dict(
                os.environ,
                {
                    "APP_SANDBOX_CONTAINER_ID": "inherited-sandbox",
                    "XPC_SERVICE_NAME": "inherited-service",
                    "DYLD_INSERT_LIBRARIES": "/tmp/unsafe.dylib",
                },
            ):
                environment = RUNNER.attempt_environment(
                    attempt_root, real_home=real_home
                )

            self.assertTrue(environment["TMPDIR"].startswith(str(attempt_root)))
            self.assertEqual(environment["HOME"], str(attempt_root / "home"))
            self.assertEqual(environment["CFFIXED_USER_HOME"], environment["HOME"])
            self.assertTrue(
                environment["DARWIN_USER_CACHE_DIR"].startswith(environment["HOME"])
            )
            self.assertTrue(Path(environment["XDG_CACHE_HOME"]).is_dir())
            self.assertTrue(Path(environment["CLANG_MODULE_CACHE_PATH"]).is_dir())
            self.assertTrue(Path(environment["SWIFT_MODULECACHE_PATH"]).is_dir())
            self.assertNotIn("APP_SANDBOX_CONTAINER_ID", environment)
            self.assertNotIn("XPC_SERVICE_NAME", environment)
            self.assertNotIn("DYLD_INSERT_LIBRARIES", environment)
            self.assertEqual(
                (Path(environment["HOME"]) / "Library/Developer/CoreSimulator/Devices").resolve(),
                simulator_devices.resolve(),
            )
            self.assertFalse(
                (Path(environment["HOME"]) / "Library/Developer/CoreSimulator/Caches").exists()
            )
            self.assertEqual(
                (Path(environment["HOME"]) / "Library/Developer/Xcode/UserData").resolve(),
                user_data.resolve(),
            )

    def test_host_context_uses_current_aqua_manager(self):
        context = RUNNER.inspect_host_context(
            system="Darwin",
            uid=502,
            probe=self.probe(
                {
                    ("getconf", "DARWIN_USER_CACHE_DIR"): (0, "/var/cache/\n", ""),
                    ("launchctl", "manageruid"): (0, "502\n", ""),
                    ("launchctl", "managername"): (0, "Aqua\n", ""),
                }
            ),
        )

        self.assertTrue(context.aqua_available)
        self.assertTrue(context.darwin_cache_available)
        self.assertEqual(context.launch_prefix, ())
        self.assertEqual(context.warnings, ())

    def test_host_context_enters_available_user_bootstrap(self):
        context = RUNNER.inspect_host_context(
            system="Darwin",
            uid=502,
            probe=self.probe(
                {
                    ("getconf", "DARWIN_USER_CACHE_DIR"): (
                        71,
                        "",
                        "confstr: Input/output error",
                    ),
                    ("launchctl", "manageruid"): (153, "", "no manager"),
                    ("launchctl", "managername"): (153, "", "no manager"),
                    ("launchctl", "asuser", "502", "/usr/bin/true"): (0, "", ""),
                }
            ),
        )

        self.assertTrue(context.aqua_available)
        self.assertFalse(context.darwin_cache_available)
        self.assertEqual(context.launch_prefix, ("launchctl", "asuser", "502"))
        self.assertTrue(any("Core Foundation cache fallback" in item for item in context.warnings))

    def test_missing_user_bootstrap_is_classified_and_disables_retry(self):
        context = RUNNER.inspect_host_context(
            system="Darwin",
            uid=502,
            probe=self.probe(
                {
                    ("getconf", "DARWIN_USER_CACHE_DIR"): (
                        71,
                        "",
                        "confstr: Input/output error",
                    ),
                    ("launchctl", "manageruid"): (153, "", "no manager"),
                    ("launchctl", "managername"): (153, "", "no manager"),
                    ("launchctl", "asuser", "502", "/usr/bin/true"): (
                        141,
                        "",
                        "Reentrancy avoided",
                    ),
                }
            ),
        )

        self.assertFalse(context.aqua_available)
        self.assertEqual(context.launch_prefix, ())
        self.assertTrue(any("identical host-abort retry is disabled" in item for item in context.warnings))

    def test_run_manifest_is_atomic_and_preserves_correlation(self):
        with tempfile.TemporaryDirectory(prefix="magios-run-manifest-") as temp:
            path = Path(temp) / "aggregate" / "ios-artifact.json"
            payload = {
                "run_id": "ios-run-123",
                "correlation_id": "aggregate-run:ios",
                "report_path": "/tmp/Magios-ios-run-123.html",
            }

            RUNNER.atomic_write_json(path, payload)

            self.assertEqual(json.loads(path.read_text(encoding="utf-8")), payload)

    def test_runner_lock_rejects_concurrent_report_owner(self):
        with tempfile.TemporaryDirectory(prefix="magios-runner-lock-") as temp:
            lock_path = Path(temp) / ".runner.lock"

            def contested_lock(_descriptor, operation):
                if operation & RUNNER.fcntl.LOCK_NB:
                    raise BlockingIOError

            with patch.object(RUNNER.fcntl, "flock", side_effect=contested_lock):
                with self.assertRaises(RUNNER.RunnerBusyError):
                    with RUNNER.exclusive_runner_lock(lock_path, 0):
                        self.fail("contested report owner unexpectedly acquired the lock")

    def test_runner_lock_rejects_non_finite_timeouts(self):
        with tempfile.TemporaryDirectory(prefix="magios-runner-lock-timeout-") as temp:
            lock_path = Path(temp) / ".runner.lock"
            for timeout in (float("nan"), float("inf")):
                with self.subTest(timeout=timeout):
                    with self.assertRaises(ValueError):
                        with RUNNER.exclusive_runner_lock(lock_path, timeout):
                            self.fail("non-finite lock timeout unexpectedly acquired the lock")

    def test_shell_status_normalizes_signaled_subprocesses(self):
        self.assertEqual(RUNNER.shell_status(-signal.SIGKILL), 128 + signal.SIGKILL)
        self.assertEqual(RUNNER.shell_status(65), 65)

    def test_diagnostic_watch_caps_only_descendant_simctl_diagnose(self):
        processes = RUNNER.parse_process_table(
            """
            100 1 /usr/bin/xcodebuild test
            101 100 /usr/bin/helper
            102 101 /usr/bin/simctl diagnose --timeout=600
            200 1 /usr/bin/simctl diagnose --timeout=600
            """
        )
        states = {}

        self.assertEqual(
            RUNNER.diagnostic_watch_actions(
                root_pid=100,
                processes=processes,
                states=states,
                now=10,
                timeout_seconds=30,
            ),
            [],
        )
        self.assertEqual(
            RUNNER.diagnostic_watch_actions(
                root_pid=100,
                processes=processes,
                states=states,
                now=40,
                timeout_seconds=30,
            ),
            [(102, signal.SIGTERM)],
        )
        self.assertNotIn(200, states)
        self.assertEqual(
            RUNNER.diagnostic_watch_actions(
                root_pid=100,
                processes=processes,
                states=states,
                now=42,
                timeout_seconds=30,
            ),
            [(102, signal.SIGKILL)],
        )

    def test_diagnostic_watch_forgets_finished_processes(self):
        states = {102: RUNNER.DiagnosticProcessState(first_seen=10)}

        actions = RUNNER.diagnostic_watch_actions(
            root_pid=100,
            processes={100: (1, "xcodebuild")},
            states=states,
            now=50,
            timeout_seconds=30,
        )

        self.assertEqual(actions, [])
        self.assertEqual(states, {})

    def test_failed_seed_preflight_propagates_status_after_quarantining_latest(self):
        with tempfile.TemporaryDirectory(prefix="magios-seed-preflight-") as temp:
            root = Path(temp)
            report_dir = root / "coverage"
            report_dir.mkdir()
            latest = report_dir / "latest.html"
            latest.write_text("old report", encoding="utf-8")
            seed_script = root / "fail-seed.sh"
            seed_script.write_text("exit 23\n", encoding="utf-8")

            status = RUNNER.main(
                [
                    "--project",
                    "Magios.xcodeproj",
                    "--scheme",
                    "Magios",
                    "--destination",
                    "platform=iOS Simulator,name=iPhone 17 Pro",
                    "--report-dir",
                    str(report_dir),
                    "--xcodebuild",
                    "/usr/bin/true",
                    "--seed-script",
                    str(seed_script),
                ]
            )

            self.assertEqual(status, 23)
            self.assertFalse(latest.exists())
            self.assertEqual(
                (report_dir / "previous.html").read_text(encoding="utf-8"),
                "old report",
            )

    def test_missing_xcode_skips_only_after_quarantining_latest(self):
        with tempfile.TemporaryDirectory(prefix="magios-no-xcode-") as temp:
            report_dir = Path(temp) / "coverage"
            report_dir.mkdir()
            latest = report_dir / "latest.html"
            latest.write_text("old report", encoding="utf-8")

            status = RUNNER.main(
                [
                    "--project",
                    "Magios.xcodeproj",
                    "--scheme",
                    "Magios",
                    "--destination",
                    "platform=iOS Simulator,name=iPhone 17 Pro",
                    "--report-dir",
                    str(report_dir),
                    "--xcodebuild",
                    "magios-definitely-missing-xcodebuild",
                ]
            )

            self.assertEqual(status, 0)
            self.assertFalse(latest.exists())
            self.assertEqual(
                (report_dir / "previous.html").read_text(encoding="utf-8"),
                "old report",
            )


if __name__ == "__main__":
    unittest.main()

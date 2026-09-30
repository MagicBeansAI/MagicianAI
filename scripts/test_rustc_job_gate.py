#!/usr/bin/env python3
"""Tests for scripts/rustc-job-gate.

Run: python3 scripts/test_rustc_job_gate.py
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import tempfile
import time
import types
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
GATE = HERE / "rustc-job-gate"


def _load_gate():
    module = types.ModuleType("rustc_job_gate")
    code = GATE.read_text(encoding="utf-8")
    exec(compile(code, str(GATE), "exec"), module.__dict__)
    return module


gate = _load_gate()


class NextHopTests(unittest.TestCase):
    def test_next_hop_does_not_reexec_self(self):
        old_argv = sys.argv[:]
        old_next = os.environ.get("RUSTC_JOB_GATE_NEXT")
        try:
            sys.argv = [str(GATE), "rustc"]
            os.environ["RUSTC_JOB_GATE_NEXT"] = str(GATE)
            self.assertEqual(gate.next_hop(["rustc", "-vV"]), ["rustc", "-vV"])
        finally:
            sys.argv = old_argv
            if old_next is None:
                os.environ.pop("RUSTC_JOB_GATE_NEXT", None)
            else:
                os.environ["RUSTC_JOB_GATE_NEXT"] = old_next


class IsCompileInvocationTests(unittest.TestCase):
    def test_probes_are_not_gated(self):
        for args in (
            ["-vV"],
            ["-V"],
            ["--version"],
            ["--print", "sysroot"],
            ["--print=cfg"],
            ["--help"],
            ["-h"],
            ["--explain", "E0308"],
        ):
            with self.subTest(args=args):
                self.assertFalse(gate.is_compile_invocation(args))

    def test_real_compile_is_gated(self):
        self.assertTrue(
            gate.is_compile_invocation(
                [
                    "--crate-name",
                    "magician",
                    "--edition",
                    "2021",
                    "--emit=dep-info,link",
                    "-C",
                    "opt-level=0",
                ]
            )
        )


class SlotGateTests(unittest.TestCase):
    def test_chained_wrapper_preserves_cargo_jobserver_descriptors(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            wrapper = root / "fake-cache"
            wrapper.write_text(
                "#!/usr/bin/env python3\n"
                "import os, sys\n"
                "os.execv(sys.argv[1], sys.argv[1:])\n"
            )
            wrapper.chmod(0o755)
            read_fd, write_fd = os.pipe()
            env = dict(
                os.environ,
                RUSTC_JOB_GATE_SLOTS="1",
                RUSTC_JOB_GATE_DIR=str(root / "slots"),
                RUSTC_JOB_GATE_NEXT=str(wrapper),
                JOBSERVER_WRITE_FD=str(write_fd),
            )
            try:
                completed = subprocess.run(
                    [sys.executable, str(GATE), sys.executable, "-c",
                     "import os; os.write(int(os.environ['JOBSERVER_WRITE_FD']), b'X')"],
                    env=env, pass_fds=(write_fd,), timeout=5, check=False,
                )
                self.assertEqual(completed.returncode, 0)
                self.assertEqual(os.read(read_fd, 1), b"X")
            finally:
                os.close(read_fd)
                os.close(write_fd)

    def test_chained_wrapper_forwards_termination_and_releases_slot(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            child_pid = root / "child.pid"
            wrapper = root / "fake-cache"
            wrapper.write_text(
                "#!/usr/bin/env python3\n"
                "import os, sys\n"
                "os.execv(sys.argv[1], sys.argv[1:])\n"
            )
            wrapper.chmod(0o755)
            env = dict(
                os.environ,
                RUSTC_JOB_GATE_SLOTS="1",
                RUSTC_JOB_GATE_DIR=str(root / "slots"),
                RUSTC_JOB_GATE_NEXT=str(wrapper),
                CHILD_PID=str(child_pid),
            )
            compiler = (
                "import os, pathlib, time; "
                "pathlib.Path(os.environ['CHILD_PID']).write_text(str(os.getpid())); "
                "time.sleep(30)"
            )
            running = subprocess.Popen(
                [sys.executable, str(GATE), sys.executable, "-c", compiler], env=env
            )
            try:
                deadline = time.monotonic() + 5
                while not child_pid.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(child_pid.exists(), "cache child did not start")
                running.terminate()
                self.assertEqual(running.wait(timeout=5), 128 + signal.SIGTERM)
                with self.assertRaises(ProcessLookupError):
                    os.kill(int(child_pid.read_text()), 0)
                completed = subprocess.run(
                    [sys.executable, str(GATE), sys.executable, "-c", "raise SystemExit(7)"],
                    env=env, timeout=5, check=False,
                )
                self.assertEqual(completed.returncode, 7)
            finally:
                if running.poll() is None:
                    running.kill()
                    running.wait()
                if child_pid.exists():
                    try:
                        os.kill(int(child_pid.read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass

    def test_cache_daemon_does_not_inherit_the_only_slot(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            daemon_pid = root / "daemon.pid"
            wrapper = root / "fake-cache"
            wrapper.write_text(
                "#!/usr/bin/env python3\n"
                "import os, pathlib, subprocess, sys, time\n"
                "pid = os.fork()\n"
                "if pid == 0:\n"
                "    time.sleep(30)\n"
                "    os._exit(0)\n"
                "pathlib.Path(os.environ['DAEMON_PID']).write_text(str(pid))\n"
                "raise SystemExit(subprocess.call(sys.argv[1:]))\n"
            )
            wrapper.chmod(0o755)
            env = dict(
                os.environ,
                RUSTC_JOB_GATE_SLOTS="1",
                RUSTC_JOB_GATE_DIR=str(root / "slots"),
                RUSTC_JOB_GATE_NEXT=str(wrapper),
                DAEMON_PID=str(daemon_pid),
            )
            command = [sys.executable, str(GATE), sys.executable, "-c", "pass"]
            try:
                first = subprocess.run(command, env=env, timeout=5, check=False)
                self.assertEqual(first.returncode, 0)
                os.kill(int(daemon_pid.read_text()), 0)
                second = subprocess.run(
                    command,
                    env=dict(env, RUSTC_JOB_GATE_NEXT=""),
                    timeout=3,
                    check=False,
                )
                self.assertEqual(second.returncode, 0)
            finally:
                if daemon_pid.exists():
                    try:
                        os.kill(int(daemon_pid.read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass

    def test_second_compile_waits_for_single_slot(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            fake_rustc = tmp_path / "fake-rustc"
            fake_rustc.write_text(
                "#!/usr/bin/env python3\n"
                "import os, pathlib, time\n"
                "marker = pathlib.Path(os.environ['GATE_MARKER'])\n"
                "go = pathlib.Path(os.environ['GATE_GO'])\n"
                "marker.write_text('running')\n"
                "while not go.exists():\n"
                "    time.sleep(0.02)\n"
                "marker.write_text('done')\n"
            )
            fake_rustc.chmod(0o755)

            env = os.environ.copy()
            env["RUSTC_JOB_GATE_SLOTS"] = "1"
            env["RUSTC_JOB_GATE_DIR"] = str(tmp_path / "slots")
            env["RUSTC_JOB_GATE_NEXT"] = ""
            env.pop("SCCACHE", None)

            first_marker = tmp_path / "first"
            second_marker = tmp_path / "second"
            go = tmp_path / "go"

            first_env = dict(env, GATE_MARKER=str(first_marker), GATE_GO=str(go))
            second_env = dict(env, GATE_MARKER=str(second_marker), GATE_GO=str(go))

            first = subprocess.Popen(
                [sys.executable, str(GATE), str(fake_rustc), "--crate-name", "one"],
                env=first_env,
            )
            deadline = time.time() + 5
            while not first_marker.exists():
                if time.time() > deadline:
                    first.kill()
                    self.fail("first compile never acquired a slot")
                time.sleep(0.02)

            second = subprocess.Popen(
                [sys.executable, str(GATE), str(fake_rustc), "--crate-name", "two"],
                env=second_env,
            )
            time.sleep(0.3)
            self.assertFalse(
                second_marker.exists(),
                "second compile ran while the only slot was held",
            )
            self.assertIsNone(second.poll(), "second compile exited instead of waiting")

            go.write_text("go")
            try:
                self.assertEqual(first.wait(timeout=5), 0)
                self.assertEqual(second.wait(timeout=5), 0)
            finally:
                if first.poll() is None:
                    first.kill()
                if second.poll() is None:
                    second.kill()
            self.assertEqual(second_marker.read_text(), "done")

    def test_version_probe_does_not_take_a_slot(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            fake_rustc = tmp_path / "fake-rustc"
            fake_rustc.write_text(
                "#!/usr/bin/env python3\n"
                "import os, pathlib, sys, time\n"
                "if '-vV' in sys.argv:\n"
                "    pathlib.Path(os.environ['PROBE_MARKER']).write_text('probed')\n"
                "    raise SystemExit(0)\n"
                "go = pathlib.Path(os.environ['GATE_GO'])\n"
                "pathlib.Path(os.environ['GATE_MARKER']).write_text('running')\n"
                "while not go.exists():\n"
                "    time.sleep(0.02)\n"
            )
            fake_rustc.chmod(0o755)

            env = os.environ.copy()
            env["RUSTC_JOB_GATE_SLOTS"] = "1"
            env["RUSTC_JOB_GATE_DIR"] = str(tmp_path / "slots")
            env["RUSTC_JOB_GATE_NEXT"] = ""
            compile_marker = tmp_path / "compile"
            probe_marker = tmp_path / "probe"
            go = tmp_path / "go"
            compile_env = dict(env, GATE_MARKER=str(compile_marker), GATE_GO=str(go))
            probe_env = dict(env, PROBE_MARKER=str(probe_marker), GATE_GO=str(go))

            held = subprocess.Popen(
                [sys.executable, str(GATE), str(fake_rustc), "--crate-name", "held"],
                env=compile_env,
            )
            deadline = time.time() + 5
            while not compile_marker.exists():
                if time.time() > deadline:
                    held.kill()
                    self.fail("compile never acquired a slot")
                time.sleep(0.02)

            probe = subprocess.run(
                [sys.executable, str(GATE), str(fake_rustc), "-vV"],
                env=probe_env,
                timeout=5,
                check=False,
            )
            self.assertEqual(probe.returncode, 0)
            self.assertEqual(probe_marker.read_text(), "probed")
            go.write_text("go")
            self.assertEqual(held.wait(timeout=5), 0)


if __name__ == "__main__":
    unittest.main()

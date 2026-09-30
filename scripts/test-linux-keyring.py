#!/usr/bin/env python3
"""Focused launcher tests; Linux additionally exercises the real Secret Service."""
import importlib.util
import io
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("run-linux-keyring.py")
spec = importlib.util.spec_from_file_location("linux_keyring", SCRIPT)
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


class Inputs(unittest.TestCase):
    def test_mount_readiness_observes_retry_without_permission_changes(self):
        state = Path('/fixture/keyring')
        with patch.object(Path, 'exists', return_value=False), \
             patch.object(launcher.os, 'access', side_effect=[False, True]), \
             patch.object(launcher.time, 'sleep') as sleep:
            launcher.wait_for_mount_access(state, '/fixture/secret', None)
        sleep.assert_called_once_with(0.1)

    def test_inaccessible_mounts_time_out(self):
        with patch.object(Path, 'exists', return_value=False), \
             patch.object(launcher.os, 'access', return_value=False), \
             patch.object(launcher.time, 'monotonic', side_effect=[0, 6]):
            with self.assertRaisesRegex(launcher.KeyringError, 'not accessible'):
                launcher.wait_for_mount_access(Path('/fixture/keyring'), '/fixture/secret', None)

    def test_private_state_preserves_existing_contents(self):
        with tempfile.TemporaryDirectory() as root:
            state = Path(root) / "state"
            launcher.private_directory(state)
            (state / "sentinel").write_text("preserve")
            launcher.private_directory(state)
            self.assertEqual((state / "sentinel").read_text(), "preserve")

    def test_public_or_symlink_state_is_rejected(self):
        with tempfile.TemporaryDirectory() as root:
            state = Path(root) / "state"
            state.mkdir(mode=0o755)
            with self.assertRaises(launcher.KeyringError):
                launcher.private_directory(state)
            link = Path(root) / "link"
            link.symlink_to(state)
            with self.assertRaises(launcher.KeyringError):
                launcher.private_directory(link)

    def test_password_file_is_private_regular_and_not_symlinked(self):
        with tempfile.TemporaryDirectory() as root:
            password = Path(root) / "password"
            value = secrets.token_bytes(32).hex().encode()
            password.write_bytes(value)
            password.chmod(0o600)
            self.assertEqual(launcher.read_password(password, None), value)
            password.chmod(0o644)
            with self.assertRaises(launcher.KeyringError):
                launcher.read_password(password, None)
            link = Path(root) / "link"
            link.symlink_to(password)
            with self.assertRaises(OSError):
                launcher.read_password(link, None)

    def test_password_input_is_bounded_and_nonempty(self):
        for value in (b"", b"short", b"a" * 4097, b"a" * 32 + b"\0"):
            with self.subTest(size=len(value)), self.assertRaises(launcher.KeyringError):
                launcher.read_password(None, io.BytesIO(value))


LIVE = sys.platform == "linux" and os.getuid() != 0 and all(
    shutil.which(name) for name in ("dbus-daemon", "dbus-send", "gnome-keyring-daemon", "secret-tool")
)


@unittest.skipUnless(LIVE, "requires non-root Linux with the packaged Secret Service tools")
class SecretService(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="magician-keyring-test-")
        self.root = Path(self.temp.name)
        self.state = self.root / "state"
        self.password = self.root / "password"
        self.password.write_text(secrets.token_hex(32))
        self.password.chmod(0o600)
        self.env = {**os.environ, "MAGICIAN_ROOT_DIR": str(self.root / "runtime")}

    def tearDown(self):
        self.temp.cleanup()

    def command(self, code):
        return [sys.executable, str(SCRIPT), "--state-dir", str(self.state),
                "--password-file", str(self.password), "--", sys.executable, "-c", code]

    def invoke(self, code):
        return subprocess.run(self.command(code), env=self.env, text=True,
                              capture_output=True, timeout=35)

    def test_reopen_keeps_credentials_and_uses_a_new_bus(self):
        writer = self.invoke("""import os,subprocess
subprocess.run(['secret-tool','store','--label=Magician test','application','magician-keyring-test'],input='fixture-secret',text=True,check=True,timeout=5)
print(os.environ['DBUS_SESSION_BUS_ADDRESS'])
""")
        self.assertEqual(writer.returncode, 0, writer.stderr)
        reader = self.invoke("""import os,subprocess
r=subprocess.run(['secret-tool','lookup','application','magician-keyring-test'],text=True,capture_output=True,check=True,timeout=5)
assert r.stdout.strip() == 'fixture-secret'
print(os.environ['DBUS_SESSION_BUS_ADDRESS'])
""")
        self.assertEqual(reader.returncode, 0, reader.stderr)
        self.assertNotEqual(writer.stdout.splitlines()[-1], reader.stdout.splitlines()[-1])
        self.assertNotIn("fixture-secret", writer.stdout + reader.stdout)

    def test_wrong_secret_never_starts_application_or_replaces_keyring(self):
        self.assertEqual(self.invoke("pass").returncode, 0)
        existing = (self.state / "data/keyrings/login.keyring").read_bytes()
        self.password.write_text(secrets.token_hex(32))
        result = self.invoke("print('APPLICATION_STARTED')")
        self.assertEqual(result.returncode, 1)
        self.assertIn("keyring is locked", result.stderr)
        self.assertNotIn("APPLICATION_STARTED", result.stdout)
        self.assertEqual((self.state / "data/keyrings/login.keyring").read_bytes(), existing)

    def test_application_exit_code_is_preserved(self):
        result = self.invoke("raise SystemExit(23)")
        self.assertEqual(result.returncode, 23, result.stderr)

    def test_application_creates_private_runtime_directories(self):
        result = self.invoke("""import os,pathlib,stat
p=pathlib.Path(os.environ['MAGICIAN_ROOT_DIR'])/'system'
p.mkdir(parents=True)
assert stat.S_IMODE(p.stat().st_mode)==0o700
""")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_state_inside_runtime_volume_is_rejected(self):
        self.state = Path(self.env["MAGICIAN_ROOT_DIR"]) / "keyring"
        result = self.invoke("print('APPLICATION_STARTED')")
        self.assertEqual(result.returncode, 1)
        self.assertIn("outside the application", result.stderr)
        self.assertNotIn("APPLICATION_STARTED", result.stdout)

    def test_termination_reaches_application_before_secret_service_stops(self):
        ready = self.root / "ready"
        acknowledged = self.root / "acknowledged"
        code = f"""import signal,time,subprocess,pathlib
def finish(*_):
    subprocess.run(['secret-tool','store','--label=Shutdown probe','application','magician-keyring-test'],input='shutdown',text=True,check=True,timeout=5)
    pathlib.Path({str(acknowledged)!r}).write_text('saved')
    raise SystemExit(0)
signal.signal(signal.SIGTERM,finish)
pathlib.Path({str(ready)!r}).touch()
while True: time.sleep(0.1)
"""
        process = subprocess.Popen(self.command(code), env=self.env, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(ready.exists())
            process.send_signal(signal.SIGTERM)
            _, error = process.communicate(timeout=15)
            self.assertEqual(process.returncode, 0, error)
            self.assertEqual(acknowledged.read_text(), "saved")
        finally:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""CUA platform/relay regressions. No downloads, daemons or GUI actions."""
import contextlib
import ctypes
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


controller = load("cua_controller", ROOT / "skillshub/macos-ui-automation/bin/macos-ui-controller")
setup = load("cua_setup", ROOT / "scripts/setup-cua-driver.py")


def completed(code=0, stdout=""):
    return subprocess.CompletedProcess([], code, stdout, "")


class PlatformTests(unittest.TestCase):
    def test_windows_official_path_is_found_without_path_refresh(self):
        with tempfile.TemporaryDirectory() as root:
            binary = Path(root) / "Programs/Cua/cua-driver/bin/cua-driver.exe"
            binary.parent.mkdir(parents=True)
            binary.write_text("fixture")
            binary.chmod(0o700)
            with patch.object(controller.sys, "platform", "win32"), patch.dict(os.environ, {"LOCALAPPDATA": root}, clear=True), patch.object(controller.shutil, "which", return_value=None):
                self.assertEqual(controller.driver_binary(), str(binary))

    def test_explicit_binary_path_wins(self):
        with tempfile.TemporaryDirectory() as root:
            binary = Path(root) / "custom driver"
            binary.write_text("fixture")
            binary.chmod(0o700)
            with patch.dict(os.environ, {"MAGICIAN_CUA_DRIVER_BIN": str(binary)}):
                self.assertEqual(controller.driver_command("status"), [str(binary), "status"])

    def test_linux_needs_x11_wayland_or_an_existing_daemon(self):
        with patch.object(controller.sys, "platform", "linux"), patch.object(controller, "driver_binary", return_value="/driver"), patch.object(controller, "daemon_status", return_value=completed(1)) as status:
            for env, expected in [({}, False), ({"DISPLAY": ":0"}, True), ({"WAYLAND_DISPLAY": "wayland-0"}, True)]:
                with patch.dict(os.environ, env, clear=True):
                    self.assertEqual(controller.local_cua_driver_available(), expected)
            with patch.dict(os.environ, {}, clear=True):
                status.return_value = completed()
                self.assertTrue(controller.local_cua_driver_available())

    def test_windows_service_session_is_not_a_desktop(self):
        def session(value):
            def set_session(pid, out):
                out._obj.value = value
                return 1
            return types.SimpleNamespace(kernel32=types.SimpleNamespace(ProcessIdToSessionId=set_session))
        for value, expected in [(0, False), (2, True)]:
            with patch.object(controller.sys, "platform", "win32"), patch.object(ctypes, "windll", session(value), create=True):
                self.assertEqual(controller.has_desktop_session(), expected)

    def test_native_linux_and_windows_start_without_mac_open(self):
        for platform in ["linux", "win32"]:
            with patch.object(controller.sys, "platform", platform), patch.object(controller, "has_desktop_session", return_value=True), patch.object(controller, "driver_binary", return_value="/driver"), patch.object(controller, "daemon_status", side_effect=[completed(1), completed()]), patch.object(controller.subprocess, "Popen") as spawn, patch.object(controller, "run") as run, patch.object(subprocess, "DETACHED_PROCESS", 8, create=True), patch.object(subprocess, "CREATE_NEW_PROCESS_GROUP", 512, create=True):
                self.assertTrue(controller.ensure_daemon())
                self.assertEqual(spawn.call_args.args[0], ["/driver", "serve"])
                self.assertEqual("creationflags" in spawn.call_args.kwargs, platform == "win32")
                run.assert_not_called()

    def test_mac_keeps_app_permission_identity(self):
        with patch.object(controller.sys, "platform", "darwin"), patch.object(controller.os.path, "isdir", return_value=True), patch.object(controller, "daemon_status", side_effect=[completed(1), completed()]), patch.object(controller, "run", return_value=completed()) as run, patch.object(controller.subprocess, "Popen") as spawn:
            self.assertTrue(controller.ensure_daemon())
            self.assertEqual(run.call_args.args[0], ["/usr/bin/open", "-n", "-g", "-a", "CuaDriver", "--args", "serve"])
            spawn.assert_not_called()

    def test_headless_start_does_not_spawn(self):
        with patch.object(controller, "daemon_status", return_value=completed(1)), patch.object(controller, "has_desktop_session", return_value=False), patch.object(controller.subprocess, "Popen") as spawn, contextlib.redirect_stderr(io.StringIO()):
            self.assertFalse(controller.ensure_daemon())
            spawn.assert_not_called()

    def test_headless_controller_relays_canonical_action(self):
        with patch.object(controller, "parse_request", return_value=("list_apps", "{}", None)), patch.object(controller, "local_cua_driver_available", return_value=False), patch.object(controller, "relay_to_host_gateway", return_value=7) as relay, patch.object(controller, "ensure_daemon") as start:
            self.assertEqual(controller.main(), 7)
            relay.assert_called_once_with("list_apps", "{}")
            start.assert_not_called()

    def test_failed_daemon_start_does_not_send_input(self):
        with patch.object(controller, "parse_request", return_value=("click", "{}", None)), patch.object(controller, "local_cua_driver_available", return_value=True), patch.object(controller, "ensure_daemon", return_value=False), patch.object(controller, "call_cua") as call:
            self.assertEqual(controller.main(), 1)
            call.assert_not_called()


class SetupTests(unittest.TestCase):
    def setUp(self):
        self.runtime = {"driver_binary": Mock(return_value="/driver"), "has_desktop_session": Mock(return_value=True), "ensure_daemon": Mock(return_value=True), "daemon_status": Mock(return_value=completed())}

    def invoke(self, args, version=setup.CUA_DRIVER_VERSION):
        with patch.object(setup.runpy, "run_path", return_value=self.runtime), patch.object(setup, "installed_version", return_value=version), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return setup.main(args)

    def test_other_driver_version_is_replaced_by_the_pin(self):
        for version in ["0.1.9", "0.28.5-nightly.20260925.1", None]:
            with patch.object(setup, "install") as install:
                # The mocked binary keeps reporting the old version, so the
                # post-install check refuses it rather than claiming success.
                self.assertEqual(self.invoke([], version=version), 1)
                install.assert_called_once()

    def test_read_only_check_fails_on_version_drift_without_installing(self):
        with patch.object(setup, "install") as install, patch.object(setup.subprocess, "run") as run:
            self.assertEqual(self.invoke(["--check"], version="0.1.9"), 1)
            install.assert_not_called()
            run.assert_not_called()

    def fake_download(self, bodies):
        def urlopen(url, timeout, context=None):
            response = Mock()
            response.__enter__ = Mock(return_value=response)
            response.__exit__ = Mock(return_value=False)
            response.read.return_value = bodies[url.rsplit("/", 1)[1]]
            return response
        return urlopen

    def pinned_hashes(self, bodies):
        return {key: tuple((name, base, setup.hashlib.sha256(bodies[name]).hexdigest()) for name, base, _ in files) for key, files in setup.INSTALLER_FILES.items() if files[0][0] in bodies}

    def test_installer_runs_pinned_scripts_side_by_side(self):
        bodies = {"install.sh": b"entry", "_install-rust.sh": b"rust", "_install-common.sh": b"common"}
        seen = {}

        def run(command, check, env):
            folder = Path(command[-1]).parent
            seen.update(files=sorted(p.name for p in folder.iterdir()), pin=env["CUA_DRIVER_RS_VERSION"])
            return completed()
        with patch.dict(setup.INSTALLER_FILES, self.pinned_hashes(bodies)), patch.object(setup.urllib.request, "urlopen", side_effect=self.fake_download(bodies)), patch.object(setup.subprocess, "run", side_effect=run), patch.object(setup.shutil, "which", side_effect=lambda name: name):
            setup.install("darwin")
        self.assertEqual(seen["files"], sorted(bodies))
        self.assertEqual(seen["pin"], setup.CUA_DRIVER_VERSION)

    def test_tampered_installer_never_executes(self):
        bodies = {"install.sh": b"entry", "_install-rust.sh": b"rust", "_install-common.sh": b"common"}
        with patch.object(setup.urllib.request, "urlopen", side_effect=self.fake_download(bodies)), patch.object(setup.subprocess, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "pinned"):
                setup.install("linux")
            run.assert_not_called()

    def test_windows_installer_loads_the_verified_module(self):
        bodies = {"install.ps1": b"Import-Bootstrap -LocalDir $PSScriptRoot -Url x", "_install-common.psm1": b"module"}
        seen = {}

        def run(command, check, env):
            entry = Path(command[-1].split("ReadAllText('")[1].split("')")[0].replace("''", "'"))
            seen["script"] = entry.read_text(encoding="utf-8")
            seen["folder"] = str(entry.parent)
            return completed()
        with patch.dict(setup.INSTALLER_FILES, self.pinned_hashes(bodies)), patch.object(setup.urllib.request, "urlopen", side_effect=self.fake_download(bodies)), patch.object(setup.subprocess, "run", side_effect=run), patch.object(setup.shutil, "which", side_effect=lambda name: name):
            setup.install("win32")
        self.assertNotIn("$PSScriptRoot", seen["script"])
        self.assertIn(f"-LocalDir '{seen['folder']}'", seen["script"])

    def test_version_parse_reads_the_driver_semver(self):
        for stdout, expected in [("0.28.3\n", "0.28.3"), ("cua-driver 0.28.5-nightly.20260925.36094786616\n", "0.28.5-nightly.20260925.36094786616"), ("", None)]:
            with patch.object(setup.subprocess, "run", return_value=completed(stdout=stdout)):
                self.assertEqual(setup.installed_version("/driver"), expected)

    def test_existing_install_is_not_replaced_or_started(self):
        with patch.object(setup, "install") as install:
            self.assertEqual(self.invoke([]), 0)
            install.assert_not_called()
            self.runtime["ensure_daemon"].assert_not_called()

    def test_headless_start_never_downloads(self):
        self.runtime["has_desktop_session"].return_value = False
        with patch.object(setup, "install") as install:
            self.assertEqual(self.invoke(["--start"]), 1)
            install.assert_not_called()

    def test_headless_install_is_allowed_without_advertising_or_starting_cua(self):
        self.runtime["has_desktop_session"].return_value = False
        self.runtime["driver_binary"].side_effect = [None, "/driver"]
        with patch.object(setup, "install") as install:
            self.assertEqual(self.invoke([]), 0)
            install.assert_called_once()
            self.runtime["ensure_daemon"].assert_not_called()

    def test_windows_and_linux_choose_different_official_installers(self):
        with patch.object(setup.shutil, "which", side_effect=lambda name: name):
            windows = setup.installer_command("win32", Path("test's folder/install.ps1"))
            self.assertEqual(windows[0], "powershell.exe")
            self.assertIn("test''s folder", windows[-1])
            self.assertNotIn("ExecutionPolicy", " ".join(windows))
            self.assertEqual(setup.installer_command("linux", Path("install.sh")), ["bash", "install.sh"])

    def test_read_only_check_does_not_install_or_start(self):
        with patch.object(setup, "install") as install, patch.object(setup.subprocess, "run", side_effect=[completed(), completed(stdout='{"windows":[{"id":1}]}')]):
            self.assertEqual(self.invoke(["--check"]), 0)
            install.assert_not_called()
            self.runtime["ensure_daemon"].assert_not_called()

    def test_doctor_zero_alone_is_not_readiness(self):
        self.runtime["daemon_status"].return_value = completed(1)
        with patch.object(setup.subprocess, "run", return_value=completed()) as run:
            self.assertEqual(self.invoke(["--check"]), 1)
            self.assertEqual(run.call_count, 1)

    def test_cli_doctor_warning_does_not_override_daemon_observation(self):
        with patch.object(setup.subprocess, "run", side_effect=[completed(1), completed(stdout='{"windows":[{"id":1}]}')]):
            self.assertEqual(self.invoke(["--check"]), 0)

    def test_empty_or_failed_window_observation_does_not_pass(self):
        for result in [completed(stdout='{"windows":[]}'), completed(1, '{"windows":[1]}'), completed(stdout="unexpected output")]:
            with patch.object(setup.subprocess, "run", side_effect=[completed(), result]):
                self.assertEqual(self.invoke(["--check"]), 1)

    def test_relay_check_is_read_only_and_checks_nested_status(self):
        self.runtime.update(host_gateway_url=lambda: "http://127.0.0.1:3017", read_bounded_response=lambda response: response.read())
        for running in [True, False]:
            response = Mock()
            response.__enter__ = Mock(return_value=response)
            response.__exit__ = Mock(return_value=False)
            response.read.return_value = json.dumps({"ok": True, "stdout": json.dumps({"running": running})}).encode()
            with patch.object(setup.urllib.request, "urlopen", return_value=response) as request, patch.object(setup, "install") as install:
                self.assertEqual(self.invoke(["--check", "--relay"]), 0 if running else 1)
                self.assertTrue(request.call_args.args[0].full_url.endswith("/host/ax/status"))
                install.assert_not_called()


def catalog_cua_pin(text):
    """The `cua-driver` driver of setup_catalog.yaml, read with stdlib only.

    Parses just the block shape the catalog uses: `version:` plus two lists of
    `- name/url/sha256` entries under `unix_installer:`/`windows_installer:`.
    """
    lines = text.splitlines()
    start = lines.index("  cua-driver:")
    block = []
    for line in lines[start + 1:]:
        if line.strip() and not line.startswith("    "):
            break
        block.append(line)
    version, lists, current = None, {}, None
    for line in block:
        stripped = line.strip()
        if stripped.startswith("version:"):
            version = stripped.split(":", 1)[1].strip().strip('"')
        elif stripped in ("unix_installer:", "windows_installer:"):
            current = lists.setdefault(stripped[:-1], [])
        elif current is not None and stripped.startswith("- name:"):
            current.append({"name": stripped.split(":", 1)[1].strip()})
        elif current is not None and ":" in stripped and current and not stripped.startswith("#"):
            key, value = stripped.split(":", 1)
            if key in ("url", "sha256"):
                current[-1][key] = value.strip()
    return version, lists


class CatalogPinTests(unittest.TestCase):
    def test_components_catalog_pins_the_same_release(self):
        text = (ROOT / "magician-components/src/setup_catalog.yaml").read_text(encoding="utf-8")
        version, lists = catalog_cua_pin(text)
        self.assertEqual(version, setup.CUA_DRIVER_VERSION)
        expected = {
            "unix_installer": [{"name": name, "url": base + name, "sha256": sha256} for name, base, sha256 in setup.INSTALLER_FILES["unix"]],
            "windows_installer": [{"name": name, "url": base + name, "sha256": sha256} for name, base, sha256 in setup.INSTALLER_FILES["win32"]],
        }
        # Desktop onboarding installs from the catalog, this script from its
        # constants; a bump must change both or neither.
        self.assertEqual(lists, expected)

    def test_catalog_parser_sees_a_drifted_hash(self):
        text = (ROOT / "magician-components/src/setup_catalog.yaml").read_text(encoding="utf-8")
        first = setup.INSTALLER_FILES["unix"][0][2]
        _, lists = catalog_cua_pin(text.replace(first, "0" * 64, 1))
        self.assertEqual(lists["unix_installer"][0]["sha256"], "0" * 64)


if __name__ == "__main__":
    unittest.main()

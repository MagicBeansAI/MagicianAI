"""Hermetic source-installer orchestration and native-service ownership checks."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

REPO = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("native_runtime_service", REPO / "scripts/native-runtime-service.py")
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)


class NativeServiceTests(unittest.TestCase):
    def test_desktop_preflight_cli_is_read_only_and_needs_no_install_prefix(self):
        with mock.patch.object(sys, "argv", ["native-runtime-service.py", "check-desktop", "--data-dir", "/fixture/notes"]), \
             mock.patch.object(sys, "platform", "darwin"), \
             mock.patch.object(native, "check_desktop_gateway") as preflight:
            self.assertEqual(native.main(), 0)
            preflight.assert_called_once_with()

    def test_desktop_restart_refuses_an_unrelated_gateway_listener(self):
        executable = "/fixture/Magican.app/Contents/MacOS/magician-desktop.bin"
        def runner(args, **kwargs):
            output = "301\n" if args[0] == "/usr/sbin/lsof" else executable + "\n"
            return subprocess.CompletedProcess(args, 0, output, "")
        native.check_desktop_gateway(runner=runner)
        executable = "/usr/bin/python3"
        with self.assertRaisesRegex(ValueError, "another application"):
            native.check_desktop_gateway(runner=runner)

    def test_desktop_restart_accepts_a_free_port_but_not_an_unreadable_listener_check(self):
        def free(args, **kwargs):
            return subprocess.CompletedProcess(args, 1, "", "")
        native.check_desktop_gateway(runner=free)
        def denied(args, **kwargs):
            return subprocess.CompletedProcess(args, 2, "", "permission denied")
        with self.assertRaisesRegex(ValueError, "could not check"):
            native.check_desktop_gateway(runner=denied)

    def test_overlapping_and_symlinked_roots_are_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "notes"
            root.mkdir()
            link = Path(temporary) / "software"
            link.symlink_to(root, target_is_directory=True)
            for prefix in (root, root / "nested", root.parent, link.resolve()):
                with self.assertRaises(ValueError):
                    native.validate_paths(prefix, root)

    def test_seeding_preserves_owner_files_and_creates_private_envs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "notes"
            root.mkdir()
            config = root / "magician-config.yaml"
            config.write_text("owner-config\n")
            env = root / ".env"
            env.write_text("OWNER=keep\n")
            native.seed(REPO, root)
            self.assertEqual(config.read_text(), "owner-config\n")
            self.assertEqual(env.read_text(), "OWNER=keep\n")
            self.assertEqual((root / ".env.development").stat().st_mode & 0o777, 0o600)
            self.assertTrue((root / "decision-engine.yaml").is_file())
            before = {path.relative_to(root): path.read_bytes() for path in root.rglob("*") if path.is_file()}
            native.seed(REPO, root)
            after = {path.relative_to(root): path.read_bytes() for path in root.rglob("*") if path.is_file()}
            self.assertEqual(before, after)

    def test_service_reuses_the_same_binding_but_rebinds_a_changed_data_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            prefix = base / "software with spaces"
            prefix.mkdir()
            for name in ("magician.bin", "magicutor.bin", "magic-supervisor.bin", "MANIFEST.yaml", "tool-runtime-config.yaml", "scripts/run-native-backend.sh", "scripts/run-ollama.sh"):
                (prefix / name).parent.mkdir(parents=True, exist_ok=True)
                (prefix / name).write_text("fixture")
            root = base / "notes"
            native.seed(REPO, root)
            service = base / "agent.plist"
            calls = []

            def runner(args, **kwargs):
                calls.append(args)
                return subprocess.CompletedProcess(args, 0, "", "")

            native.start(prefix, root, runner=runner, service_path=service)
            definition = plistlib.loads(service.read_bytes())
            self.assertEqual(definition["ProgramArguments"], ["/bin/bash", str(prefix / "scripts/run-native-backend.sh")])
            self.assertEqual(definition["EnvironmentVariables"]["MAGICIAN_ROOT_DIR"], str(root))
            actions = lambda: [call[1] for call in calls if call[0] == "/bin/launchctl" and call[1] != "print"]
            self.assertEqual(actions(), ["bootout", "enable", "bootstrap", "kickstart"])
            calls.clear()
            native.start(prefix, root, runner=runner, service_path=service)
            self.assertEqual(actions(), ["kickstart"])
            other = base / "other notes"
            native.seed(REPO, other)
            calls.clear()
            native.start(prefix, other, runner=runner, service_path=service)
            self.assertIn("bootstrap", [call[1] for call in calls])
            self.assertEqual(plistlib.loads(service.read_bytes())["EnvironmentVariables"]["MAGICIAN_ROOT_DIR"], str(other))
            for file in (prefix / "magician.bin", other / ".env.development"):
                file.write_text(file.read_text() + "\nupdated")
                calls.clear()
                native.start(prefix, other, runner=runner, service_path=service)
                self.assertIn("bootstrap", [call[1] for call in calls], "updated software or credentials must reach the service")

    def test_interrupted_seed_never_publishes_a_partial_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "notes/config.yaml"
            def interrupted(input_file, output):
                output.write(b"partial")
                raise OSError("interrupted copy")
            with mock.patch.object(native.shutil, "copyfileobj", interrupted):
                with self.assertRaises(OSError):
                    native.copy_absent(REPO / "magician-config.yaml", destination)
            self.assertFalse(destination.exists())
            self.assertEqual(list(destination.parent.iterdir()), [])
            native.copy_absent(REPO / "magician-config.yaml", destination)
            self.assertEqual(destination.read_bytes(), (REPO / "magician-config.yaml").read_bytes())

    def test_failed_registration_is_not_success_and_missing_config_does_not_touch_services(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            prefix = base / "software"
            prefix.mkdir()
            for name in ("magician.bin", "magicutor.bin", "magic-supervisor.bin", "MANIFEST.yaml", "tool-runtime-config.yaml", "scripts/run-native-backend.sh", "scripts/run-ollama.sh"):
                (prefix / name).parent.mkdir(parents=True, exist_ok=True)
                (prefix / name).write_text("fixture")
            root = base / "notes"
            calls = []

            def runner(args, **kwargs):
                calls.append(args)
                return subprocess.CompletedProcess(args, 1, "", "cannot register")

            with self.assertRaisesRegex(ValueError, "configuration is missing"):
                native.start(prefix, root, runner=runner, service_path=base / "agent.plist")
            self.assertEqual(calls, [])
            native.seed(REPO, root)
            with self.assertRaisesRegex(ValueError, "enable failed"):
                native.start(prefix, root, runner=runner, service_path=base / "agent.plist")

    def test_foreign_listeners_are_refused_but_owned_children_are_allowed(self):
        def runner(args, **kwargs):
            output = ""
            if args[0] == "/bin/launchctl": output = "pid = 42\n"
            if args[0] == "/bin/ps": output = "PID PPID\n42 1\n43 42\n44 43\n"
            if args[0] == "/usr/sbin/lsof": output = "44\n"
            return subprocess.CompletedProcess(args, 0, output, "")
        native.check_service_ports(runner, "gui/501/fixture")
        native.check_service_ports(runner, "gui/501/fixture", require_bound=True)
        def foreign(args, **kwargs):
            result = runner(args, **kwargs)
            if args[0] == "/usr/sbin/lsof": result.stdout = "99\n"
            return result
        with self.assertRaisesRegex(ValueError, "port 8081"):
            native.check_service_ports(foreign, "gui/501/fixture")

    def test_readiness_requires_the_reviewed_binding_and_its_actual_listeners(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            prefix, root = base / "software", base / "notes"
            native.seed(REPO, root)
            service = base / "agent.plist"
            service.write_bytes(plistlib.dumps(native.service_definition(prefix, root)))
            def runner(args, **kwargs):
                output = "pid = 42\n" if args[0] == "/bin/launchctl" else ""
                if args[0] == "/bin/ps": output = "42 1\n43 42\n"
                if args[0] == "/usr/sbin/lsof": output = "43\n"
                return subprocess.CompletedProcess(args, 0, output, "")
            native.verify_binding(prefix, root, runner=runner, service_path=service)
            with self.assertRaisesRegex(ValueError, "different installation"):
                native.verify_binding(prefix, base / "other notes", runner=runner, service_path=service)
            def not_started(args, **kwargs):
                return subprocess.CompletedProcess(args, 1, "", "")
            with self.assertRaisesRegex(ValueError, "not bound"):
                native.verify_binding(prefix, root, runner=not_started, service_path=service)


class SourceInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.bin = self.base / "bin"
        self.bin.mkdir()
        self.journal = self.base / "commands.jsonl"
        self.toolchains = self.base / "toolchains.jsonl"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ.get('PATH', '')}", SOURCE_TEST_JOURNAL=str(self.journal), SOURCE_TEST_BASE=str(self.base),
                        SOURCE_TEST_TOOLCHAINS=str(self.toolchains), DECISION_ENGINE_MLX="1", MLX_RUST_TOOLCHAIN="1.95.0")
        self.command("uname", '#!/bin/sh\ncase "$1" in -m) echo arm64;; *) echo Darwin;; esac\n')
        self.command("sw_vers", "#!/bin/sh\necho 14.5\n")
        self.command("make", f'''#!{sys.executable}
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ['SOURCE_TEST_JOURNAL'], 'a') as f: f.write(json.dumps(args)+'\\n')
with open(os.environ['SOURCE_TEST_TOOLCHAINS'], 'a') as f:
    f.write(json.dumps(dict(args=args, rust=os.environ.get('RUSTUP_TOOLCHAIN'), mlx=os.environ.get('MLX_RUST_TOOLCHAIN')))+'\\n')
if os.environ.get('SOURCE_TEST_FAIL') in args: sys.exit(7)
if 'package-source' in args:
    target = next(arg.split('=', 1)[1] for arg in args if arg.startswith('MAGICIAN_PACKAGE_PATH_FILE='))
    package = pathlib.Path(os.environ['SOURCE_TEST_BASE']) / 'source-test-package'
    package.mkdir(exist_ok=True)
    (package / 'SHA256SUMS').write_text('fixture')
    (package / 'install.sh').write_text('#!/bin/sh\\nexit 0\\n')
    pathlib.Path(target).write_text(str(package) + '.tar.gz\\n')
''')

    def command(self, name, contents):
        path = self.bin / name
        path.write_text(contents)
        path.chmod(0o755)

    def run_install(self, *args):
        return subprocess.run(["/bin/bash", str(REPO / "scripts/install-source-checkout.sh"),
                               "--data-dir", str(self.base / "notes"), "--prefix", str(self.base / "software"), *args],
                              env=self.env, text=True, capture_output=True)

    def test_dry_run_does_not_create_data_prefix_answers_or_execute_make(self):
        result = self.run_install("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.base / "notes").exists())
        self.assertFalse((self.base / "software").exists())
        self.assertFalse(self.journal.exists())
        for stage in ("build-source-debug", "package-source", "native-runtime-service.py", "install-verify.sh --core"):
            self.assertIn(stage, result.stdout)

    def test_preparation_builds_once_and_never_launches_a_runtime(self):
        result = self.run_install("--phase", "prepare")
        self.assertEqual(result.returncode, 0, result.stderr)
        commands = [json.loads(line) for line in self.journal.read_text().splitlines()]
        builds = [args for args in commands if "build-source-debug" in args]
        self.assertEqual(len(builds), 1)
        self.assertIn("build-source-debug", builds[0])
        self.assertFalse(any("restart-desktop-tray-debug" in args for args in commands))
        setup_index = next(i for i, args in enumerate(commands) if "setup-decision-engine-mlx-build" in args)
        builds_with_env = [json.loads(line) for line in self.toolchains.read_text().splitlines()]
        for call in builds_with_env[setup_index + 1:]:
            self.assertEqual(call["rust"], "1.95.0")
            self.assertEqual(call["mlx"], "1.95.0")

    def test_missing_mlx_prerequisites_fail_before_runtime_build_and_data_seeding(self):
        self.env["SOURCE_TEST_FAIL"] = "setup-decision-engine-mlx-build"
        result = self.run_install("--phase", "prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.base / "notes").exists())
        self.assertNotIn('"build-source-debug"', self.journal.read_text())

    def test_failed_build_stops_before_packaging_or_startup(self):
        self.env["SOURCE_TEST_FAIL"] = "build-source-debug"
        result = self.run_install("--phase", "prepare")
        self.assertNotEqual(result.returncode, 0)
        commands = self.journal.read_text()
        self.assertNotIn('"package-source"', commands)
        self.assertNotIn('"restart-desktop-tray-debug"', commands)

    def test_desktop_installer_does_not_restart_itself_but_terminal_setup_uses_new_build(self):
        result = self.run_install("--phase", "start", "--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("check-desktop", result.stdout)
        self.assertIn("restart-desktop-tray-debug", result.stdout)
        result = self.run_install("--phase", "start", "--dry-run", "--desktop-running")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("check-desktop", result.stdout)
        self.assertNotIn("restart-desktop-tray-debug", result.stdout)
        self.assertIn("native-runtime-service.py", result.stdout)

    def test_unsupported_host_is_refused_before_mutation(self):
        self.command("uname", "#!/bin/sh\necho Linux\n")
        result = self.run_install("--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.base / "notes").exists())
        self.assertFalse(self.journal.exists())


class CoreVerificationTests(unittest.TestCase):
    command = SourceInstallerTests.command

    def setUp(self):
        SourceInstallerTests.setUp(self)
        self.env["MAGICIAN_PI_VERSION"] = "1.2.3"
        self.command("pi", "#!/bin/sh\necho 1.2.3\n")
        self.command("curl", f'''#!{sys.executable}
import json, os, pathlib, sys
url = sys.argv[-1]
with open(os.environ['SOURCE_TEST_JOURNAL'], 'a') as f: f.write(json.dumps(url)+'\\n')
fault = os.environ.get('SOURCE_TEST_UNHEALTHY')
if fault and fault in url: sys.exit(22)
if url.endswith('/api/ps'):
    print('{{"models": []}}' if os.environ.get('SOURCE_TEST_EMPTY_MEMORY') else '{{"models": [{{"name": "embedder"}}]}}')
''')

    def verify(self, *args):
        return subprocess.run(["/bin/bash", str(REPO / "scripts/install-verify.sh"), "--core", *args],
                              env=self.env, text=True, capture_output=True)

    def test_core_requires_memory_and_gateway_but_not_generation(self):
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        probes = self.journal.read_text()
        self.assertNotIn("11434", probes)
        self.assertIn("3003/health", probes)
        self.assertIn("3017/host/automation/status", probes)
        self.env["SOURCE_TEST_EMPTY_MEMORY"] = "1"
        self.assertNotEqual(self.verify().returncode, 0)
        self.env.pop("SOURCE_TEST_EMPTY_MEMORY")
        self.env["SOURCE_TEST_UNHEALTHY"] = "3017"
        self.assertNotEqual(self.verify().returncode, 0)

    def test_magicutor_errors_and_wrong_pi_version_are_failures(self):
        self.env["SOURCE_TEST_UNHEALTHY"] = "3003"
        self.assertNotEqual(self.verify().returncode, 0)
        self.env.pop("SOURCE_TEST_UNHEALTHY")
        self.command("pi", "#!/bin/sh\necho 0.0.1\n")
        self.assertNotEqual(self.verify().returncode, 0)

    def test_wait_is_bounded_and_rejects_invalid_arguments(self):
        for value in ("invalid", "601", "-1"):
            self.assertEqual(self.verify("--wait", value).returncode, 2)
        self.env["SOURCE_TEST_UNHEALTHY"] = "3003"
        self.assertNotEqual(self.verify("--wait", "1").returncode, 0)


if __name__ == "__main__":
    unittest.main()

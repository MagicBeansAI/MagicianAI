#!/usr/bin/env python3
"""Build-router regression checks; fixtures never compile Rust or touch runtime data."""
import contextlib
import fcntl
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("build_cache", ROOT / "scripts/build-cache.py")
cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cache)
import build_cache_defaults as defaults


class BuildRouting(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.cargo = self.root / "cargo-real"
        self.mbx = self.root / "mbx"
        for executable in (self.cargo, self.mbx):
            executable.write_text("#!/bin/sh\nexit 0\n")
            executable.chmod(0o755)
        self.env = {"PATH": "/usr/bin:/bin", "MAGICIAN_REAL_CARGO": str(self.cargo),
                    "MAGICIAN_BUILD_CACHE": "mbx", "MBX_BIN": str(self.mbx),
                    "CARGO_TARGET_DIR": str(self.root / "target"),
                    "MAGICIAN_CARGO_MANAGED_VIEW": str(self.root / "target"),
                    "MAGICIAN_CARGO_WORKSPACE": str(self.root),
                    "RUSTC_WRAPPER": "/gate", "CARGO_INCREMENTAL": "1"}

    def plan(self, args, primary=True):
        with patch.object(cache, "is_primary_workspace", return_value=primary), \
             patch.object(cache, "target_root", return_value=self.root / "store/targets"):
            return cache.cargo_plan(args, self.env)

    def test_managed_dev_build_keeps_gate_and_incremental_without_double_cache(self):
        self.env["RUSTC_JOB_GATE_NEXT"] = "/sccache"
        command, env = self.plan(["+stable", "build", "--locked"])
        self.assertEqual(command, [str(self.mbx), "+stable", "build", "--locked"])
        self.assertNotIn("CARGO_TARGET_DIR", env)
        self.assertEqual(env["MBX_EAGER_INCREMENTAL"], "1")
        self.assertNotIn("MBX_LEARNED_INCREMENTAL", env)
        self.assertEqual(env["RUSTC_WRAPPER"], "/gate")
        self.assertEqual(env["RUSTC_JOB_GATE_NEXT"], "")
        self.assertEqual(env["CARGO"], str(self.cargo))

    def test_release_does_not_enable_incremental(self):
        _, env = self.plan(["build", "--release"])
        self.assertNotIn("MBX_EAGER_INCREMENTAL", env)

    def test_explicit_incremental_opt_out_reaches_mbx_for_dev_commands(self):
        self.env["CARGO_INCREMENTAL"] = "0"
        for command in ("check", "build", "test", "clippy"):
            with self.subTest(command=command):
                _, env = self.plan([command])
                self.assertEqual(env["CARGO_INCREMENTAL"], "0")
                self.assertEqual(env["MBX_EAGER_INCREMENTAL"], "0")
                self.assertEqual(env["MBX_LEARNED_INCREMENTAL"], "0")

    def test_explicit_incremental_opt_out_reaches_explicit_profiles(self):
        self.env["CARGO_INCREMENTAL"] = "0"
        for args in (["build", "--release"], ["build", "--profile", "dev"]):
            with self.subTest(args=args):
                _, env = self.plan(args)
                self.assertEqual(env["MBX_LEARNED_INCREMENTAL"], "0")
                self.assertNotIn("MBX_EAGER_INCREMENTAL", env)

    def test_explicit_mbx_incremental_policy_is_preserved(self):
        for cargo_mode, mbx_mode in (("1", "0"), ("0", "1")):
            with self.subTest(cargo_mode=cargo_mode, mbx_mode=mbx_mode):
                self.env.update(CARGO_INCREMENTAL=cargo_mode,
                                MBX_EAGER_INCREMENTAL=mbx_mode,
                                MBX_LEARNED_INCREMENTAL=mbx_mode)
                _, env = self.plan(["check"])
                self.assertEqual(env["MBX_EAGER_INCREMENTAL"], mbx_mode)
                self.assertEqual(env["MBX_LEARNED_INCREMENTAL"], mbx_mode)

    def test_gate_is_below_mbx_instead_of_disabling_rust_cache(self):
        gate = self.root / "gate"
        gate.write_text("#!/bin/sh\nexit 0\n")
        gate.chmod(0o755)
        self.env.update(RUSTC_JOB_GATE=str(gate), RUSTC_WRAPPER=str(gate))
        with patch.object(cache.shutil, "which", return_value="/fake/rustup"), \
             patch.object(cache.subprocess, "check_output", side_effect=["/real/rustc\n", "/real/rustdoc\n"]):
            _, env = self.plan(["+stable", "check"])
        self.assertEqual(env["RUSTC_WRAPPER"], "")
        self.assertEqual(env["MAGICIAN_REAL_RUSTC"], "/real/rustc")
        self.assertEqual(env["RUSTC"], str(ROOT / "scripts/build-tools/rustc-gated"))

    def test_explicit_target_and_foreign_workspace_keep_artifact_paths(self):
        for args, primary in [(["check", "--target-dir", "/chosen"], True),
                              (["build", "--manifest-path", "desktop/Cargo.toml"], False)]:
            _, env = self.plan(args, primary)
            self.assertEqual(env["CARGO_TARGET_DIR"], self.env["CARGO_TARGET_DIR"])
        self.env["CARGO_TARGET_DIR"] = "/other-target"
        _, env = self.plan(["check"])
        self.assertEqual(env["CARGO_TARGET_DIR"], "/other-target")

    def test_queries_and_clean_keep_cargo_semantics(self):
        for args in (["--version"], ["metadata", "--no-deps"], ["clean", "-p", "magician"], ["fmt"]):
            command, env = self.plan(args)
            self.assertEqual(command[0], str(self.cargo))
            self.assertEqual(env["CARGO_TARGET_DIR"], self.env["CARGO_TARGET_DIR"])

    def test_required_volume_stops_before_target_or_tmp_writes(self):
        self.env["MAGICIAN_BUILD_REQUIRED_VOLUME"] = str(self.root / "unmounted")
        self.env["MAGICIAN_BUILD_TMPDIR"] = str(self.root / "compiler-tmp")
        with self.assertRaisesRegex(RuntimeError, "not mounted"):
            self.plan(["build"])
        self.assertFalse((self.root / "compiler-tmp").exists())
        self.assertFalse((self.root / "target").exists())

    def test_old_target_outputs_survive_and_failed_placement_restores_link(self):
        old = self.root / "old-output"
        old.mkdir()
        (old / "artifact").write_text("retained")
        (self.root / "target").symlink_to(old, target_is_directory=True)
        with contextlib.redirect_stderr(io.StringIO()):
            _, env = self.plan(["build"])
            self.assertFalse((self.root / "target").exists())
            self.assertEqual(cache.launch(["/usr/bin/false"], env), 1)
        self.assertEqual((self.root / "target").resolve(), old.resolve())
        self.assertEqual((old / "artifact").read_text(), "retained")

    def test_active_old_build_blocks_migration(self):
        old = self.root / "old-output/debug"
        old.mkdir(parents=True)
        (self.root / "target").symlink_to(old.parent, target_is_directory=True)
        with (old / ".cargo-lock").open("a+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.assertFalse(cache.managed_ready(self.root))
            with self.assertRaisesRegex(RuntimeError, "Cargo is using"):
                self.plan(["build"])
        self.assertTrue((self.root / "target").is_symlink())

    def test_direct_make_recipe_uses_router_on_macos_make_381(self):
        # A fake Cargo records native wiring. If make resolves a bare `cargo`
        # from its incoming PATH instead of the exported PATH, this assertion
        # fails without compiling anything.
        self.cargo.write_text('#!/usr/bin/env python3\nimport os,json\nprint(json.dumps({"native":os.getenv("HOST_CXX")}))\n')
        env = os.environ.copy()
        env["PATH"] = cache.clean_path(env)
        for key in list(env):
            if key.startswith(("MAGICIAN_", "MBX_")) or key in {"CARGO", "CC", "CXX", "HOST_CC", "HOST_CXX", "CC_KNOWN_WRAPPER_CUSTOM", "CARGO_TARGET_DIR", "CARGO_TARGET_ROOT"}:
                env.pop(key)
        output = subprocess.check_output(["make", "--no-print-directory", "check-runtime-core",
                                          "MAGICIAN_BUILD_CONFIG=/dev/null", "MAGICIAN_BUILD_CACHE=off",
                                          f"MAGICIAN_BUILD_STORAGE_ROOT={self.root}/cache",
                                          f"MAGICIAN_REAL_CARGO={self.cargo}", f"SCCACHE={self.mbx}"],
                                         cwd=ROOT, env=env, text=True)
        self.assertIn("sccache-cxx", json.loads(output.splitlines()[-1])["native"])

    def test_plain_native_cache_preserves_rust_incremental_and_user_compilers(self):
        fake = self.root / "sccache-real"
        fake.write_text("#!/bin/sh\nexit 0\n")
        fake.chmod(0o755)
        self.env.update(MAGICIAN_BUILD_CACHE="off", SCCACHE=str(fake))
        command, env = self.plan(["build"])
        self.assertEqual(command[0], str(self.cargo))
        self.assertEqual(env["CARGO_INCREMENTAL"], "1")
        self.assertIn("sccache-cxx", env["HOST_CXX"])
        self.env["HOST_CXX"] = "/chosen/compiler"
        _, env = self.plan(["build"])
        self.assertEqual(env["HOST_CXX"], "/chosen/compiler")

    def test_native_proxy_removes_only_incremental_override(self):
        fake = self.root / "sccache-record"
        fake.write_text("#!/usr/bin/env python3\nimport json,os,sys\nprint(json.dumps({\"args\":sys.argv[1:],\"incremental\":os.getenv(\"CARGO_INCREMENTAL\"),\"rustflags\":os.getenv(\"RUSTFLAGS\")}))\n")
        fake.chmod(0o755)
        env = os.environ.copy()
        env.update(SCCACHE=str(fake), CARGO_INCREMENTAL="1", RUSTFLAGS="-C debuginfo=1")
        result = subprocess.check_output([str(ROOT / "scripts/build-tools/sccache-cxx"),
                                          "/usr/bin/clang++", "-c", "source.cpp"], env=env, text=True)
        recorded = json.loads(result)
        self.assertIsNone(recorded["incremental"])
        self.assertEqual(recorded["rustflags"], "-C debuginfo=1")
        self.assertEqual(recorded["args"], ["/usr/bin/clang++", "-c", "source.cpp"])

    def test_global_flags_and_toolchain_selection(self):
        self.assertEqual(cache.command_name(["+nightly", "--config", "a=b", "-Zunstable-options", "check"]), "check")

    def test_link_target_uses_worktrees_isolated_fallback(self):
        chosen = self.root / "isolated-worktree-output"
        env = {"MAGICIAN_BUILD_CACHE": "off", "MAGICIAN_CARGO_UNMANAGED_TARGET": str(chosen)}
        with patch.object(cache, "ROOT", self.root), patch.dict(os.environ, env, clear=True), \
             patch.object(cache.sys, "argv", ["build-cache.py", "link-target"]), \
             contextlib.redirect_stdout(io.StringIO()):
            cache.main()
        self.assertEqual((self.root / "target").resolve(), chosen.resolve())

    def test_make_fallback_and_explicit_override(self):
        env = os.environ.copy()
        env["PATH"] = cache.clean_path(env)
        for key in list(env):
            if key.startswith(("MAGICIAN_", "MBX_")) or key in {"CARGO_TARGET_DIR", "CARGO_TARGET_ROOT"}:
                env.pop(key)
        common = ["make", "--no-print-directory", "build-cache-status", "MAGICIAN_BUILD_CONFIG=/dev/null",
                  "MBX_BIN=", "MAGICIAN_BUILD_CACHE_BOOTSTRAP=0",
                  "MAGICIAN_WT_LINKED=no",
                  f"MAGICIAN_BUILD_STORAGE_ROOT={self.root}/cache",
                  f"CARGO_TARGET_ROOT_PREFERRED={self.root}/absent/disk/builds"]
        status = json.loads(subprocess.check_output(common, cwd=ROOT, env=env, text=True))
        self.assertEqual(status["backend"], "cargo")
        self.assertIsNone(status["managed_view"])
        self.assertEqual(status["target"], str(ROOT / "target"))
        linked = json.loads(subprocess.check_output(common + ["MAGICIAN_WT_LINKED=yes"],
                                                   cwd=ROOT, env=env, text=True))
        self.assertEqual(linked["backend"], "cargo")
        self.assertIsNone(linked["managed_view"])
        self.assertEqual(linked["target"], str(ROOT / "target" / "wt" / ROOT.name))
        chosen = str(self.root / "chosen")
        status = json.loads(subprocess.check_output(common + [f"MBX_BIN={self.mbx}",
                                                             f"CARGO_TARGET_DIR={chosen}"], cwd=ROOT, env=env, text=True))
        self.assertEqual(status["target"], chosen)
        self.assertIsNone(status["managed_view"])
        status = json.loads(subprocess.check_output(common + [f"MBX_BIN={self.mbx}",
                                                             f"CARGO_TARGET_ROOT={chosen}"], cwd=ROOT, env=env, text=True))
        self.assertEqual(status["target"], chosen)
        self.assertIsNone(status["managed_view"])

    def test_portable_make_defaults_need_no_host_file_or_installed_tool(self):
        env = os.environ.copy()
        env["PATH"] = cache.clean_path(env)
        for key in list(env):
            if key.startswith(("MAGICIAN_", "MBX_", "SCCACHE_")) or key in {"CARGO_TARGET_DIR", "CARGO_TARGET_ROOT", "CARGO_BUILD_JOBS", "CARGO_NET_OFFLINE"}:
                env.pop(key)
        storage = self.root / "portable-cache"
        common = ["make", "--no-print-directory", "build-cache-status", "MAGICIAN_BUILD_CONFIG=/dev/null",
                  f"MAGICIAN_BUILD_STORAGE_ROOT={storage}", "MAGICIAN_BUILD_CACHE_BOOTSTRAP=1",
                  f"CARGO_TARGET_ROOT_PREFERRED={self.root}/absent/disk/builds", "MBX_BIN="]
        status = json.loads(subprocess.check_output(common, cwd=ROOT, env=env, text=True))
        self.assertTrue(status["bootstrap_on_build"])
        self.assertEqual(status["cache"], str(storage / "mbx"))
        self.assertEqual(status["storage_root"], str(storage))
        self.assertTrue(Path(status["auxiliary_storage"]).is_relative_to(storage / "builds"))
        self.assertFalse(storage.exists())  # Inspection does not install or write.
        installed = storage / "tools/mbx" / defaults.VERSION / "bin/mbx"
        installed.parent.mkdir(parents=True)
        installed.write_text("#!/bin/sh\nexit 0\n")
        installed.chmod(0o755)
        status = json.loads(subprocess.check_output(common[:-1], cwd=ROOT, env=env, text=True))
        self.assertEqual(status["mbx"], str(installed))
        self.assertFalse(status["bootstrap_on_build"])

    def test_bootstrap_is_build_only_and_offline_preserves_external_placement(self):
        self.env.update(MAGICIAN_BUILD_CACHE="auto", MBX_BIN="",
                        MAGICIAN_BUILD_CACHE_BOOTSTRAP="1", MAGICIAN_BUILD_STORAGE_ROOT=str(self.root / "cache"),
                        MAGICIAN_CARGO_UNMANAGED_TARGET=str(self.root / "external-output"))
        with patch.object(cache, "find_mbx", return_value=None), patch.object(cache.subprocess, "run") as run:
            command, _ = self.plan(["metadata", "--no-deps"])
            self.assertEqual(command[0], str(self.cargo))
            run.assert_not_called()
            for flags in (["--offline"], ["--frozen"]):
                command, env = self.plan(["build", *flags])
                self.assertEqual(command[0], str(self.cargo))
                self.assertEqual(env["CARGO_TARGET_DIR"], str(self.root / "target"))
                self.assertEqual((self.root / "target").resolve(), (self.root / "external-output").resolve())
                run.assert_not_called()
            self.env["CARGO_NET_OFFLINE"] = "true"
            self.plan(["build"])
            run.assert_not_called()
        self.env.pop("CARGO_NET_OFFLINE")
        with patch.object(cache, "find_mbx", side_effect=[None, str(self.mbx)]), patch.object(cache.subprocess, "run") as run:
            with contextlib.redirect_stderr(io.StringIO()):
                command, _ = self.plan(["build"])
            self.assertEqual(command[0], str(self.mbx))
            self.assertIn("setup-build-cache.py", run.call_args.args[0][1])
            self.assertNotIn("--write-config", run.call_args.args[0])


class PortableDefaults(unittest.TestCase):
    def test_only_compatible_hosts_bootstrap_prebuilt_releases(self):
        with patch.object(defaults.platform, "system", return_value="Linux"), \
             patch.object(defaults.platform, "machine", return_value="x86_64"):
            with patch.object(defaults.platform, "libc_ver", return_value=("glibc", "2.35")):
                self.assertEqual(defaults.release_host(), "x86_64-unknown-linux-gnu")
            with patch.object(defaults.platform, "libc_ver", return_value=("musl", "1.2")):
                self.assertIsNone(defaults.release_host())
        with patch.object(defaults.platform, "system", return_value="Darwin"), \
             patch.object(defaults.platform, "machine", return_value="x86_64"):
            self.assertIsNone(defaults.release_host())

    def test_storage_without_ssd_uses_platform_cache_and_honors_xdg(self):
        with patch.object(defaults.Path, "is_mount", return_value=False), \
             patch.object(defaults.Path, "home", return_value=Path("/users/example")), \
             patch.dict(os.environ, {}, clear=True):
            with patch.object(defaults.platform, "system", return_value="Darwin"):
                self.assertEqual(defaults.storage_root(), Path("/users/example/Library/Caches/magician-build"))
            with patch.object(defaults.platform, "system", return_value="Linux"):
                self.assertEqual(defaults.storage_root(), Path("/users/example/.cache/magician-build"))
            with patch.dict(os.environ, {"XDG_CACHE_HOME": "/cache/custom"}):
                self.assertEqual(defaults.storage_root(), Path("/cache/custom/magician-build"))

    def test_ssd_preference_and_small_machine_memory_budget(self):
        with patch.object(defaults.Path, "is_mount", return_value=True), \
             patch.object(defaults.os, "access", return_value=True):
            self.assertEqual(defaults.storage_root(), Path("/Volumes/build/magician"))
        with patch.object(defaults.platform, "system", return_value="Linux"), \
             patch.object(defaults.os, "sysconf", side_effect=[1024**2, 4096]):
            self.assertEqual(defaults.memory_budget(), "3GiB")

    def test_setup_installs_verified_release_without_creating_host_settings(self):
        import hashlib
        import importlib
        import tarfile
        setup = importlib.import_module("setup-build-cache")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = io.BytesIO()
            executable = b"#!/bin/sh\necho mbx-fixture\n"
            with tarfile.open(fileobj=archive, mode="w:gz") as bundle:
                entry = tarfile.TarInfo("mbx")
                entry.size = len(executable)
                bundle.addfile(entry, io.BytesIO(executable))
            packed = archive.getvalue()
            checksum = f"{hashlib.sha256(packed).hexdigest()}  mbx-aarch64-apple-darwin.tar.gz\n".encode()
            argv = ["setup-build-cache.py", "--storage-root", str(root / "cache")]
            with patch.object(cache.sys, "argv", argv), \
                 patch.object(setup, "release_host", return_value="aarch64-apple-darwin"), \
                 patch.object(setup, "download", side_effect=[checksum, packed]) as download, \
                 patch.dict(os.environ, {"MAGICIAN_BUILD_CONFIG": str(root / "host.mk")}), \
                 contextlib.redirect_stdout(io.StringIO()):
                setup.main()
                setup.main()  # Second clone reuses the installed tool.
            self.assertEqual(download.call_count, 2)
            self.assertFalse((root / "host.mk").exists())
            self.assertTrue((root / "cache/tools/rustc-gated").is_file())
            self.assertTrue((root / "cache/sccache/config.toml").is_file())


if __name__ == "__main__":
    unittest.main()

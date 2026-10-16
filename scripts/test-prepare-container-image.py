#!/usr/bin/env python3
"""Offline pipeline contracts: real git snapshots, fake builds/runtime commands."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("prepare", HERE / "prepare-container-image.py")
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)
layer_spec = importlib.util.spec_from_file_location("layer", HERE / "build-container-layer.py")
layer = importlib.util.module_from_spec(layer_spec)
layer_spec.loader.exec_module(layer)


class PipelineTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.repo, self.work = self.root / "repo", self.root / "work"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "user.name", "Fixture")
        # The skill manifest names a real skill: the tool-skill contract test
        # requires every skillshub/ path under scripts/ to resolve in the tree.
        for name in ("Cargo.lock", "Makefile", "Dockerfile", "containers/sdk/Dockerfile",
                     "magician/src/lib.rs", "ui/unified-ui/src/app.ts", "skillshub/browser/SKILL.md",
                     "scripts/container-entrypoint.sh", "scripts/install-container-tools.sh",
                     "scripts/install-container-higgsfield.py", "magician-config.yaml",
                     "magicutor/config/config.yaml"):
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture\n")
        self.commit()
        self.calls = []
        self.failure = None
        managed_env = (["NO_COLOR=true"] if "NO_COLOR" in os.environ else [])
        if "BUILDKIT_COLORS" in os.environ:
            managed_env.append("BUILDKIT_COLORS=" + os.environ["BUILDKIT_COLORS"])
        self.builder = {"status": {"state": "stopped"}, "configuration": {
            "resources": {"cpus": 1, "memoryInBytes": 12 * prepare.GIB},
            "image": {"reference": "fixture-builder:1"},
            "initProcess": {"environment": managed_env}}}
        self.output = io.StringIO()
        self.quiet = contextlib.redirect_stdout(self.output)
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def git(self, *args):
        return prepare.git(self.repo, *args)

    def commit(self):
        self.git("add", ".")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture")

    def plan(self):
        return prepare.make_plan(self.repo, self.work)

    def fake_run(self, argv, cwd=None, capture=False):
        args = list(map(str, argv))
        self.calls.append(args)
        if self.failure and self.failure(args):
            raise RuntimeError("injected failure")
        if args[:3] == ["container", "inspect", "buildkit"]:
            return subprocess.CompletedProcess(args, 0, json.dumps([self.builder]), "")
        if args[:4] == ["container", "system", "property", "list"]:
            return subprocess.CompletedProcess(args, 0, json.dumps({"build": {"image": "fixture-builder:1"}}), "")
        if args[:3] == ["container", "image", "inspect"]:
            return subprocess.CompletedProcess(args, 1, "", "not found")
        if "--output" in args:
            output = Path(args[args.index("--output") + 1])
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(b"fixture archive")
            receipt = {"sha256": prepare.digest(output)}
            if "--revision" in args:
                receipt["source_revision"] = args[args.index("--revision") + 1]
            Path(str(output) + ".json").write_text(json.dumps(receipt))
        return subprocess.CompletedProcess(args, 0, "", "")

    def release(self, name):
        release = self.work / "releases" / name
        release.mkdir(parents=True)
        return release

    def execute(self, plan, name):
        release = self.release(name)
        prepare.execute(plan, self.repo, release, self.fake_run)
        return json.loads((release / "result.json").read_text())

    def test_committed_snapshot_ignores_parallel_edits_and_private_untracked_files(self):
        plan = self.plan()
        (self.repo / "magician/src/lib.rs").write_text("uncommitted other agent\n")
        (self.repo / ".env").write_text("PRIVATE=not-an-image-input\n")
        destination = Path(plan["source"])
        prepare.snapshot(self.repo, plan["source_revision"], destination)
        self.assertEqual((destination / "magician/src/lib.rs").read_text(), "fixture\n")
        self.assertFalse((destination / ".env").exists())

    def test_base_invalidation_tracks_installers_but_not_overlay_payloads(self):
        previous = self.plan()["base_key"]
        for filename in ["magician/src/lib.rs", "ui/unified-ui/src/app.ts",
                         "scripts/container-entrypoint.sh", "skillshub/browser/SKILL.md"]:
            with (self.repo / filename).open("a") as f:
                f.write("changed\n")
            self.commit()
            self.assertEqual(previous, self.plan()["base_key"])
        for filename in ["magician-config.yaml", "magicutor/config/config.yaml",
                         "scripts/install-container-tools.sh",
                         "scripts/install-container-higgsfield.py", "Dockerfile", "Cargo.lock"]:
            with (self.repo / filename).open("a") as f:
                f.write("changed\n")
            self.commit()
            current = self.plan()["base_key"]
            self.assertNotEqual(previous, current, filename)
            previous = current

    def test_new_pipeline_builds_runtime_base_then_host_artifacts(self):
        result = self.execute(self.plan(), "first")
        self.assertEqual(result["mode"], "full")
        self.assertTrue(result["image_loaded"])
        self.assertFalse(result["application_boot_tested"])
        self.assertFalse(result["changes_running_stack"])
        builds = [c for c in self.calls if c[:2] == ["container", "build"]]
        self.assertEqual(len(builds), 1)
        # Omitted flags would apply Apple's defaults and replace this 12 GiB VM.
        self.assertEqual(builds[0][builds[0].index("--cpus") + 1], "1")
        self.assertEqual(builds[0][builds[0].index("--memory") + 1], "12288m")
        self.assertEqual(builds[0][builds[0].index("--target") + 1], "runtime-base")
        self.assertTrue(any("build-container-artifacts.py" in " ".join(c) for c in self.calls))
        self.assertEqual(self.plan()["mode"], "artifacts")

    def test_repository_dockerfile_stays_below_apple_build_rpc_ceiling(self):
        dockerfile = HERE.parent / "Dockerfile"
        self.assertLessEqual(dockerfile.stat().st_size, prepare.APPLE_DOCKERFILE_MAX_BYTES)

    def test_runtime_base_target_excludes_application_builder_outputs(self):
        source = (HERE.parent / "Dockerfile").read_text()
        runtime_base = source.split("FROM debian:bookworm-slim AS runtime-base", 1)[1]
        runtime_base = runtime_base.split("FROM runtime-base AS runtime", 1)[0]
        self.assertNotIn("COPY --from=builder", runtime_base)
        skilltools = source.split("AS skilltools", 1)[1].split("AS agentbrowser", 1)[0]
        self.assertNotIn("FROM builder AS skilltools", source)
        self.assertIn("COPY . .", skilltools)

    def test_oversized_dockerfile_is_refused_before_builder_start(self):
        (self.repo / "Dockerfile").write_text("#" * (prepare.APPLE_DOCKERFILE_MAX_BYTES + 1))
        self.commit()
        with self.assertRaisesRegex(ValueError, "compatibility ceiling"):
            self.execute(self.plan(), "oversized-dockerfile")
        self.assertFalse(any(c[:3] == ["container", "builder", "start"] for c in self.calls))

    def test_cached_base_cross_compiles_on_host_without_sdk_or_builder(self):
        self.execute(self.plan(), "first")
        (self.repo / "magician/src/lib.rs").write_text("new service\n")
        self.commit()
        self.calls.clear()
        result = self.execute(self.plan(), "second")
        self.assertEqual(result["mode"], "artifacts")
        command = next(c for c in self.calls if "build-container-artifacts.py" in " ".join(c))
        for flag, value in [("--engine", "zig"), ("--cpus", "2"), ("--jobs", "2")]:
            self.assertEqual(command[command.index(flag) + 1], value)
        self.assertNotIn("--sdk-image", command)
        self.assertNotIn("--memory", command)
        self.assertFalse(any(c[:3] == ["container", "image", "inspect"] for c in self.calls))
        self.assertNotIn(["container", "stop", "buildkit"], self.calls)
        self.assertTrue(any(c[:3] == ["container", "image", "load"] for c in self.calls))
        self.assertTrue(any("package" in c for c in self.calls))

    def test_explicit_apple_artifact_engine_uses_sdk_and_stops_builder(self):
        self.execute(self.plan(), "first-apple-base")
        (self.repo / "magician/src/lib.rs").write_text("new service\n")
        self.commit()
        self.calls.clear()
        plan = prepare.make_plan(self.repo, self.work, artifact_engine="apple")
        self.execute(plan, "second-apple")
        compile_index = next(i for i, c in enumerate(self.calls) if "build-container-artifacts.py" in " ".join(c))
        stop_index = self.calls.index(["container", "stop", "buildkit"])
        self.assertLess(stop_index, compile_index)
        command = self.calls[compile_index]
        for flag, value in [("--engine", "apple"), ("--jobs", "1"), ("--memory", "12g")]:
            self.assertEqual(command[command.index(flag) + 1], value)

    def test_explicit_local_base_is_exported_and_never_built(self):
        plan = prepare.make_plan(self.repo, self.work, adopted_base_image="magician:qualified")
        result = self.execute(plan, "adopt")
        self.assertEqual(result["mode"], "adopt")
        self.assertFalse(any(c[:2] == ["container", "build"] for c in self.calls))
        export = next(c for c in self.calls if "export" in c)
        self.assertEqual(export[export.index("--image") + 1], "magician:qualified")
        self.assertTrue(Path(plan["base"]).is_file())
        marker = json.loads(Path(plan["base_marker"]).read_text())
        self.assertEqual(marker["adopted_image"], "magician:qualified")
        self.assertEqual(prepare.make_plan(self.repo, self.work)["mode"], "artifacts")
        replacement = prepare.make_plan(
            self.repo, self.work, adopted_base_image="magician:qualified-v2")
        self.assertEqual(replacement["mode"], "adopt")

    def test_initial_local_base_is_used_once_then_cached(self):
        first = prepare.make_plan(
            self.repo, self.work, initial_base_image="magician:qualified")
        self.assertEqual(first["mode"], "adopt")
        self.execute(first, "initial-base")
        later = prepare.make_plan(
            self.repo, self.work, initial_base_image="magician:qualified")
        self.assertEqual(later["mode"], "artifacts")
        self.assertIsNone(later["adopted_base_image"])

    def test_installer_change_rebuilds_instead_of_adopting_stale_initial_base(self):
        first = prepare.make_plan(
            self.repo, self.work, initial_base_image="magician:qualified")
        self.execute(first, "initial-runtime")
        with (self.repo / "scripts/install-container-tools.sh").open("a") as source:
            source.write("dependency installer changed\n")
        self.commit()
        changed = prepare.make_plan(
            self.repo, self.work, initial_base_image="magician:qualified")
        self.assertEqual(changed["mode"], "full")
        self.assertIsNone(changed["adopted_base_image"])

    def test_force_rebuild_from_scratch_ignores_cached_and_initial_bases(self):
        first = prepare.make_plan(
            self.repo, self.work, initial_base_image="magician:qualified")
        self.execute(first, "initial-force")
        forced = prepare.make_plan(
            self.repo, self.work, initial_base_image="magician:qualified",
            force_rebuild_from_scratch=True)
        self.assertEqual(forced["mode"], "full")
        self.assertTrue(forced["forced_rebuild_from_scratch"])
        self.assertIsNone(forced["adopted_base_image"])

    def test_pipeline_never_controls_the_running_application_or_mounts_live_data(self):
        self.execute(self.plan(), "first")
        for command in self.calls:
            if command[:2] == ["container", "stop"]:
                self.assertEqual(command[2:], ["buildkit"])
            self.assertNotIn("magician-integration-test", command)
            self.assertNotIn("--privileged", command)
            if command[:2] == ["container", "run"]:
                self.assertIn("--entrypoint", command)
                self.assertNotIn("-p", command)
                self.assertNotIn("target=/data", " ".join(command))

    def test_failed_smoke_does_not_publish_base_or_success(self):
        plan = self.plan()
        self.failure = lambda c: c[:2] == ["container", "run"]
        with self.assertRaisesRegex(RuntimeError, "injected"):
            self.execute(plan, "failure")
        self.assertFalse(Path(plan["base_marker"]).exists())
        self.assertFalse((self.work / "releases/failure/result.json").exists())
        self.assertFalse(any("export" in c for c in self.calls))

    def test_builder_owner_is_not_stopped_or_resized(self):
        self.builder["status"]["state"] = "running"
        with self.assertRaisesRegex(ValueError, "already running"):
            self.execute(self.plan(), "busy")
        self.assertNotIn(["container", "stop", "buildkit"], self.calls)
        self.assertFalse(any(c[:2] == ["container", "build"] for c in self.calls))

    def test_builder_failure_releases_only_owned_vm_and_does_not_continue(self):
        self.failure = lambda c: c[:2] == ["container", "build"]
        with self.assertRaisesRegex(RuntimeError, "injected"):
            self.execute(self.plan(), "failed-build")
        self.assertIn(["container", "stop", "buildkit"], self.calls)
        self.assertFalse(any(c[:2] == ["container", "run"] for c in self.calls))

    def test_builder_is_prestarted_and_ready_before_build(self):
        self.execute(self.plan(), "prestarted-builder")
        start = next(i for i, c in enumerate(self.calls)
                     if c[:3] == ["container", "builder", "start"])
        ready = next(i for i, c in enumerate(self.calls)
                     if c[:3] == ["container", "exec", "buildkit"])
        build = next(i for i, c in enumerate(self.calls)
                     if c[:2] == ["container", "build"])
        stop = self.calls.index(["container", "stop", "buildkit"])
        self.assertLess(start, ready)
        self.assertLess(ready, build)
        self.assertLess(build, stop)

    def test_builder_readiness_failure_releases_owned_vm(self):
        self.failure = lambda c: c[:3] == ["container", "exec", "buildkit"]
        with self.assertRaisesRegex(RuntimeError, "injected"):
            self.execute(self.plan(), "failed-readiness")
        self.assertIn(["container", "stop", "buildkit"], self.calls)
        self.assertFalse(any(c[:2] == ["container", "build"] for c in self.calls))

    def test_transient_apple_build_stream_disconnect_is_retried(self):
        failed = False

        def fail_first_build(command):
            nonlocal failed
            if command[:2] == ["container", "build"] and not failed:
                failed = True
                raise RuntimeError('unavailable: "Stream unexpectedly closed."')
            return False

        self.failure = fail_first_build
        self.execute(self.plan(), "transient-build-stream")
        builds = [c for c in self.calls if c[:2] == ["container", "build"]]
        self.assertEqual(len(builds), 2)

    def test_non_transport_build_failure_is_not_retried(self):
        self.failure = lambda c: c[:2] == ["container", "build"]
        with self.assertRaisesRegex(RuntimeError, "injected"):
            self.execute(self.plan(), "compile-failure")
        builds = [c for c in self.calls if c[:2] == ["container", "build"]]
        self.assertEqual(len(builds), 1)

    def test_fresh_builder_is_bounded_and_oversized_builder_is_refused(self):
        self.assertEqual(prepare.builder_limits(None), ["--cpus", "2", "--memory", "12g"])
        self.builder["configuration"]["resources"]["cpus"] = 8
        with self.assertRaisesRegex(ValueError, "refusing to resize"):
            prepare.builder_limits(self.builder)

    def test_builder_image_or_managed_environment_drift_is_refused_before_build(self):
        for field in ("image", "environment"):
            with self.subTest(field=field):
                self.calls.clear()
                if field == "image":
                    self.builder["configuration"]["image"]["reference"] = "old-builder:0"
                else:
                    self.builder["configuration"]["image"]["reference"] = "fixture-builder:1"
                    self.builder["configuration"]["initProcess"]["environment"] = ["NO_COLOR=unexpected"]
                with self.assertRaisesRegex(ValueError, "refusing to replace"):
                    self.execute(self.plan(), "drift-" + field)
                self.assertFalse(any(c[:2] == ["container", "build"] for c in self.calls))
                self.assertNotIn(["container", "stop", "buildkit"], self.calls)

    def test_nondefault_memory_is_preserved_without_rounding(self):
        self.builder["configuration"]["resources"] = {"cpus": 2, "memoryInBytes": 3584 * 1024 ** 2}
        self.execute(self.plan(), "nondefault-memory")
        build = next(c for c in self.calls if c[:2] == ["container", "build"])
        self.assertEqual(build[build.index("--memory") + 1], "3584m")
        self.assertEqual(build[build.index("--cpus") + 1], "2")

    def test_layer_wrapper_preserves_apple_limits_and_stops_its_builder(self):
        with patch.object(sys, "argv", ["layer", "--", "--tag", "fixture:layer", "."]), \
                patch.object(layer, "run", side_effect=self.fake_run):
            layer.main()
        build = next(c for c in self.calls if c[:2] == ["container", "build"])
        self.assertEqual(build[2:6], ["--cpus", "1", "--memory", "12288m"])
        self.assertEqual(self.calls[-1], ["container", "stop", "buildkit"])

    def test_layer_wrapper_refuses_recreation_flags_before_touching_builder(self):
        for flag in ("--memory=4g", "--cpus", "--dns"):
            self.calls.clear()
            with self.subTest(flag=flag), contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(sys, "argv", ["layer", "--", flag, "."]), \
                    patch.object(layer, "run", side_effect=self.fake_run), self.assertRaises(SystemExit):
                layer.main()
            self.assertEqual(self.calls, [])

    def test_layer_wrapper_leaves_docker_resource_options_and_apple_builder_alone(self):
        with patch.object(sys, "argv", ["layer", "--engine", "docker", "--", "--memory=4g", "."]), \
                patch.object(layer, "run", side_effect=self.fake_run):
            layer.main()
        self.assertEqual(self.calls, [["docker", "build", "--memory=4g", "."]])

    def test_dry_run_has_no_filesystem_or_container_side_effects(self):
        with patch.object(prepare, "HERE", self.repo / "scripts"), patch.object(prepare, "Runner") as runner:
            prepare.main(["--dry-run", "--work-dir", str(self.work)])
        self.assertFalse(self.work.exists())
        runner.assert_not_called()

    def test_unrecognized_source_directory_is_not_overwritten(self):
        plan = self.plan()
        source = Path(plan["source"])
        source.mkdir(parents=True)
        (source / "keep").write_text("keep")
        with self.assertRaisesRegex(ValueError, "unrecognized"):
            prepare.snapshot(self.repo, plan["source_revision"], source)
        self.assertEqual((source / "keep").read_text(), "keep")

    def test_explicit_refresh_overrides_valid_base_cache(self):
        self.execute(self.plan(), "first")
        self.assertEqual(prepare.make_plan(self.repo, self.work, refresh=True)["mode"], "full")

    def test_substituted_base_is_refused_before_compilation(self):
        self.execute(self.plan(), "first")
        plan = self.plan()
        Path(plan["base"]).write_bytes(b"different archive")
        self.calls.clear()
        with self.assertRaisesRegex(ValueError, "base digest changed"):
            self.execute(plan, "bad-cache")
        self.assertEqual(self.calls, [])

    def test_failed_archive_verification_never_loads_candidate_or_reports_success(self):
        self.execute(self.plan(), "first")
        self.failure = lambda c: "verify" in c
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "injected"):
            self.execute(self.plan(), "bad-archive")
        self.assertFalse(any(c[:3] == ["container", "image", "load"] for c in self.calls))
        self.assertFalse((self.work / "releases/bad-archive/result.json").exists())

    def test_git_snapshot_refuses_escaping_symlinks(self):
        (self.repo / "escape").symlink_to("../../outside")
        self.commit()
        plan = self.plan()
        with self.assertRaisesRegex(ValueError, "symlink escapes"):
            prepare.snapshot(self.repo, plan["source_revision"], Path(plan["source"]))
        self.assertFalse(Path(plan["source"]).exists())

    def test_archive_paths_cannot_escape_destination(self):
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w") as archive:
            member = tarfile.TarInfo("../escape")
            member.size = 1
            archive.addfile(member, io.BytesIO(b"x"))
        raw.seek(0)
        stage = self.root / "stage"
        stage.mkdir()
        with tarfile.open(fileobj=raw) as archive, self.assertRaisesRegex(ValueError, "unsafe source path"):
            prepare.extract_source(archive, stage)
        self.assertFalse((self.root / "escape").exists())


if __name__ == "__main__":
    unittest.main()

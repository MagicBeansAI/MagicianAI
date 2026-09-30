#!/usr/bin/env python3
"""Prepare a verified local Apple Container OCI image from one committed revision.

No running application container, desktop settings or runtime data is changed.
The first base can be explicitly adopted or built from the main Dockerfile.
Service/UI-only updates cross-compile on macOS by default, then use the cached
artifact/OCI lane. All expensive work is serial.
"""
import argparse
from collections import deque
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
from datetime import datetime, timezone

HERE = Path(__file__).resolve().parent
GIB = 1024 ** 3
SCHEMA = "magician.prepare-image.v1"
APPLE_DOCKERFILE_MAX_BYTES = 14_500


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return "sha256:" + hasher.hexdigest()


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args])


def base_input(path):
    # Conservative reuse: unknown paths invalidate the base. Only known pure
    # application/UI/runtime-overlay/doc inputs are excluded. The final OCI
    # overlay replaces tracked scripts, while the two installers executed by
    # Dockerfile still invalidate the dependency base.
    if path.startswith("magicutor/config/"):
        return True
    if path.startswith("scripts/"):
        return path in {
            "scripts/install-container-tools.sh",
            "scripts/install-container-higgsfield.py",
        }
    # Declarative skill manifests are copied into the final OCI artifact layer.
    # They do not change installed Linux programs, language dependencies, the
    # patched browser binary, or browser caches in runtime-base.
    if path.startswith("skillshub/") and PurePosixPath(path).name == "SKILL.md":
        return False
    application_roots = ("magician/", "magician-bin/", "magicutor/",
                         "magic-supervisor/", "magicllm/", "ui/", "sdk/typescript/",
                         "desktop/", "magios/", "magdroid/", "docs/")
    if path.startswith(application_roots) or ("/" not in path and path.endswith(".md")):
        return False
    return True


def identity(repo, revision):
    commit = git(repo, "rev-parse", "--verify", revision + "^{commit}").decode().strip()
    records = git(repo, "ls-tree", "-rz", commit).split(b"\0")
    base = hashlib.sha256(SCHEMA.encode())
    sdk_blob = None
    for record in filter(None, records):
        metadata, raw_path = record.split(b"\t", 1)
        mode, kind, blob = metadata.split()
        path = raw_path.decode()
        require(kind == b"blob", f"source contains a submodule, which git archive cannot snapshot: {path}")
        if base_input(path):
            base.update(record + b"\0")
        if path == "containers/sdk/Dockerfile":
            sdk_blob = blob.decode()
    require(sdk_blob, "selected revision predates containers/sdk/Dockerfile")
    return commit, base.hexdigest(), sdk_blob


def make_plan(repo, work, revision="HEAD", arch="arm64", refresh=False,
              artifact_engine="zig", adopted_base_image=None, initial_base_image=None,
              force_rebuild_from_scratch=False):
    commit, base_key, sdk_key = identity(repo, revision)
    base = work / "bases" / f"{arch}-{base_key}.oci.tar"
    marker = Path(str(base) + ".prepared.json")
    reuse = False
    if base.is_file() and marker.is_file() and not refresh:
        previous = json.loads(marker.read_text())
        reuse = (previous.get("schema") == SCHEMA and previous.get("base_key") == base_key
                 and previous.get("arch") == arch and bool(previous.get("sha256")))
    prior_base_cache = any((work / "bases").glob(f"{arch}-*.oci.tar.prepared.json"))
    selected_base_image = None
    if not force_rebuild_from_scratch:
        selected_base_image = adopted_base_image or (
            initial_base_image if not reuse and not refresh and not prior_base_cache else None)
    mode = ("full" if force_rebuild_from_scratch else
            "adopt" if selected_base_image else
            "artifacts" if reuse else "full")
    return dict(schema=SCHEMA, source_revision=commit, arch=arch, base_key=base_key,
                work=str(work), source=str(work / "sources" / commit), base=str(base),
                base_marker=str(marker), mode=mode, adopted_base_image=selected_base_image,
                forced_rebuild_from_scratch=force_rebuild_from_scratch,
                artifact_engine=artifact_engine,
                sdk_image=(f"magician-build-sdk:{arch}-{sdk_key[:16]}"
                           if artifact_engine == "apple" else None),
                cache=str(work / "caches" / f"{artifact_engine}-{arch}-{sdk_key}"),
                artifacts=str(work / "artifacts" / f"{arch}-{commit}-{sdk_key}.tar"),
                compile_cpus=2,
                compile_memory="12g" if artifact_engine == "apple" else None,
                cargo_jobs=2 if artifact_engine == "zig" else 1,
                application_boot_tested=False, changes_running_stack=False)


def snapshot(repo, commit, destination):
    marker = destination / ".magician-source-revision"
    if destination.exists():
        require(marker.is_file() and marker.read_text().strip() == commit,
                f"unrecognized source directory: {destination}")
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Use immutable git objects, never a moving shared working tree or live env.
    with tempfile.TemporaryDirectory(prefix=".snapshot-", dir=destination.parent) as tmp:
        stage = Path(tmp) / "source"
        stage.mkdir()
        archive = Path(tmp) / "source.tar"
        subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", "-o", str(archive), commit], check=True)
        with tarfile.open(archive) as source:
            extract_source(source, stage)
        (stage / ".magician-source-revision").write_text(commit + "\n")
        stage.rename(destination)


def extract_source(archive, stage):
    """Extract git's regular files and internal links, including on Python 3.10."""
    seen, links = set(), []
    for member in archive:
        path = PurePosixPath(member.name)
        require(not path.is_absolute() and ".." not in path.parts and path.parts,
                f"unsafe source path: {member.name}")
        require(path not in seen, f"duplicate source path: {member.name}")
        seen.add(path)
        destination = stage / path
        if member.isdir():
            destination.mkdir(parents=True, exist_ok=True)
        elif member.isfile():
            destination.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(member) as source, destination.open("xb") as target:
                shutil.copyfileobj(source, target)
            destination.chmod(0o755 if member.mode & 0o111 else 0o644)
        elif member.issym():
            require(not Path(member.linkname).is_absolute()
                    and (destination.parent / member.linkname).resolve().is_relative_to(stage.resolve()),
                    f"source symlink escapes snapshot: {member.name}")
            links.append((destination, member.linkname))
        else:
            raise ValueError(f"unsupported source file: {member.name}")
    # No archive member can write through a link: links are created last.
    for destination, target in links:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.symlink_to(target)
    for destination, _ in links:
        require(destination.resolve().is_relative_to(stage.resolve()),
                f"source symlink chain escapes snapshot: {destination.relative_to(stage)}")


def write_json(path, value):
    temporary = path.with_name(path.name + ".pending")
    with temporary.open("x") as out:
        json.dump(value, out, indent=2)
        out.write("\n")
    os.replace(temporary, path)


def builder_limits(info):
    if info is None:
        return ["--cpus", "2", "--memory", "12g"]
    require(info["status"]["state"] == "stopped",
            "Apple BuildKit is already running; wait for its owner to finish and stop it before preparing an image")
    resources = info["configuration"]["resources"]
    cpus, memory = resources["cpus"], resources["memoryInBytes"]
    require(0 < cpus <= 2 and 0 < memory <= 12 * GIB,
            "existing builder exceeds 2 CPUs/12 GiB; refusing to resize it or discard its cache")
    print(f"Preserving existing builder limits: {cpus} CPU(s), {memory / GIB:g} GiB", flush=True)
    # Apple 1.0.0 resolves omitted flags from system defaults, then recreates a
    # mismatched builder. Repeat the inspected values, not those defaults.
    require(isinstance(cpus, int) and isinstance(memory, int) and memory % (1024 ** 2) == 0,
            "builder resources cannot be represented exactly; refusing to resize it")
    return ["--cpus", str(cpus), "--memory", f"{memory // (1024 ** 2)}m"]


class Runner:
    def __init__(self, log_dir):
        self.log_dir = log_dir
        self.number = 0

    def __call__(self, argv, cwd=None, capture=False):
        argv = list(map(str, argv))
        print("+ " + shlex.join(argv), flush=True)
        if capture:
            return subprocess.run(argv, cwd=cwd, text=True, capture_output=True)
        self.number += 1
        log = self.log_dir / f"{self.number:02d}.log"
        print(f"  running; log: {log}", flush=True)
        with log.open("w") as evidence:
            result = subprocess.run(argv, cwd=cwd, stdout=evidence, stderr=subprocess.STDOUT)
        if result.returncode:
            with log.open(errors="replace") as evidence:
                tail = "".join(deque(evidence, maxlen=35))
            print(tail, file=sys.stderr)
            raise RuntimeError(f"command failed ({result.returncode}); log: {log}; output: {tail.strip()}")
        print(f"  completed; log: {log}", flush=True)
        return result


@contextlib.contextmanager
def image_builder(run):
    inspected = run(["container", "inspect", "buildkit"], capture=True)
    if inspected.returncode:
        diagnostic = inspected.stderr.lower().replace(" ", "")
        require("notfound" in diagnostic or "doesnotexist" in diagnostic,
                "cannot inspect Apple builder: " + inspected.stderr.strip())
        info = None
    else:
        objects = json.loads(inspected.stdout)
        require(isinstance(objects, list) and len(objects) == 1, "unexpected builder inspection output")
        info = objects[0]
    limits = builder_limits(info)
    if info is not None:
        properties = run(["container", "system", "property", "list", "--format", "json"], capture=True)
        require(properties.returncode == 0, "cannot inspect Apple builder image defaults")
        configured_image = json.loads(properties.stdout)["build"]["image"]
        require(info["configuration"]["image"]["reference"] == configured_image,
                "Apple builder image default changed; refusing to replace the cached builder")
        managed_env = sorted(value for value in info["configuration"]["initProcess"]["environment"]
                             if value.startswith(("BUILDKIT_COLORS=", "NO_COLOR=")))
        requested_env = []
        if "BUILDKIT_COLORS" in os.environ:
            requested_env.append("BUILDKIT_COLORS=" + os.environ["BUILDKIT_COLORS"])
        if "NO_COLOR" in os.environ:
            requested_env.append("NO_COLOR=true")
        require(managed_env == sorted(requested_env),
                "Apple builder color environment changed; refusing to replace the cached builder")
    started = False
    try:
        # Apple Container 1.0 can cancel the builder stream when `container build`
        # has to create and connect to BuildKit in one operation. Starting the
        # bounded builder first avoids that race while retaining the same cache.
        run(["container", "builder", "start", *limits])
        started = True
        run(["container", "exec", "buildkit", "sh", "-c",
             "i=0; while [ \"$i\" -lt 100 ]; do "
             "test -S /run/buildkit/buildkitd.sock && exit 0; "
             "i=$((i + 1)); sleep 0.1; done; exit 1"])
        yield limits
    finally:
        # This builder was stopped/absent on entry. Keep its caches, release VM RAM
        # before the separate artifact compiler starts. Never stop an active owner.
        if started:
            stopped = run(["container", "stop", "buildkit"], capture=True)
            if stopped.returncode:
                raise RuntimeError("Builder stop did not complete; refusing overlapping compile VMs: " + stopped.stderr.strip())


def container_build(run, argv, cwd=None, attempts=3):
    """Retry only Apple's transient build-stream disconnect, preserving cache."""
    for attempt in range(1, attempts + 1):
        try:
            return run(argv, cwd=cwd)
        except RuntimeError as error:
            transient = "stream unexpectedly closed" in str(error).lower()
            if not transient or attempt == attempts:
                raise
            print(f"Apple Container build stream closed; retrying cached build "
                  f"({attempt + 1}/{attempts}).", flush=True)


def smoke(run, image, arch):
    # No entrypoint/agents, ports, mounted runtime root, accounts or provider calls.
    # Read-only pipeline mount supplies the gate even for older Dockerfiles.
    script = """set -eu
test "$(id -u)" != 0
test -s /app/ui/index.html
for binary in /app/magician.bin /app/magicutor.bin /app/magic-supervisor; do
  test -x "$binary"
  linkage=$(ldd "$binary")
  printf '%s\\n' "$linkage"
  if printf '%s\\n' "$linkage" | grep -q 'not found'; then exit 1; fi
done
/app/skillshub/.venv/bin/python -c 'import yaml, PIL, trafilatura'
/app/skillshub/.venv/bin/python /pipeline/verify-container-skill-bins.py
"""
    run(["container", "run", "--rm", "--name", "magician-image-check-" + os.urandom(5).hex(),
         "--cpus", "2", "--memory", "512m", "--arch", arch, "--progress", "none",
         "--mount", f"type=bind,source={HERE},target=/pipeline,readonly",
         "--entrypoint", "/bin/sh", image, "-c", script])


def smoke_base(run, image, arch):
    """Qualify runtime dependencies without requiring application artifacts."""
    script = """set -eu
test "$(id -u)" != 0
test -s /app/magician-config.yaml
test -s /app/scripts/container-entrypoint.sh
/app/skillshub/.venv/bin/python -c 'import yaml, PIL, trafilatura'
/app/skillshub/.venv/bin/python /pipeline/verify-container-skill-bins.py
"""
    run(["container", "run", "--rm", "--name", "magician-base-check-" + os.urandom(5).hex(),
         "--cpus", "2", "--memory", "512m", "--arch", arch, "--progress", "none",
         "--mount", f"type=bind,source={HERE},target=/pipeline,readonly",
         "--entrypoint", "/bin/sh", image, "-c", script])


def execute(plan, repo, release, run):
    source, base = Path(plan["source"]), Path(plan["base"])
    snapshot(repo, plan["source_revision"], source)
    tag = "magician:prepared-" + release.name
    output = release / "image.oci.tar"
    oci = [sys.executable, HERE / "container-oci.py"]
    if plan["mode"] == "full":
        print("Runtime base is new/changed: rebuilding dependencies only; application artifacts stay on the host.", flush=True)
        dockerfile_bytes = (source / "Dockerfile").stat().st_size
        require(dockerfile_bytes <= APPLE_DOCKERFILE_MAX_BYTES,
                f"Dockerfile is {dockerfile_bytes} bytes; Apple Container 1.0 drops build RPCs "
                f"above the {APPLE_DOCKERFILE_MAX_BYTES}-byte compatibility ceiling")
        base_tag = tag + "-runtime-base"
        with image_builder(run) as limits:
            container_build(run, ["container", "build", *limits,
                            "--arch", plan["arch"], "--progress", "plain",
                            "--target", "runtime-base",
                            "--build-arg", "CARGO_BUILD_JOBS=1",
                            "--build-arg", "CARGO_PROFILE_RELEASE_LTO=false",
                            "--label", "org.opencontainers.image.revision=" + plan["source_revision"],
                            "--tag", base_tag, "."], cwd=source)
        smoke_base(run, base_tag, plan["arch"])
        base_output = release / "runtime-base.oci.tar"
        run([*oci, "export", "--image", base_tag, "--arch", plan["arch"], "--output", base_output])
        run([*oci, "verify", base_output, "--arch", plan["arch"]])
        base.parent.mkdir(parents=True, exist_ok=True)
        temporary = base.with_suffix(".pending")
        require(not temporary.exists(), f"unexpected pending base: {temporary}")
        os.link(base_output, temporary)
        os.replace(temporary, base)
        write_json(Path(plan["base_marker"]), dict(schema=SCHEMA, base_key=plan["base_key"],
                                                  arch=plan["arch"], source_revision=plan["source_revision"],
                                                  sha256=json.loads(Path(str(base_output) + ".json").read_text())["sha256"]))
    elif plan["mode"] == "adopt":
        print("Adopting the explicitly selected local runtime image; no Dockerfile build.", flush=True)
        base.parent.mkdir(parents=True, exist_ok=True)
        pending = Path(str(base) + ".adopt.pending")
        pending_receipt = Path(str(pending) + ".json")
        require(not pending.exists() and not pending_receipt.exists(),
                f"unexpected pending adopted base: {pending}")
        run([*oci, "export", "--image", plan["adopted_base_image"], "--arch", plan["arch"],
             "--output", pending])
        run([*oci, "verify", pending, "--arch", plan["arch"]])
        smoke_base(run, plan["adopted_base_image"], plan["arch"])
        os.replace(pending, base)
        os.replace(pending_receipt, Path(str(base) + ".json"))
        write_json(Path(plan["base_marker"]), dict(
            schema=SCHEMA, base_key=plan["base_key"], arch=plan["arch"],
            source_revision=plan["source_revision"], adopted_image=plan["adopted_base_image"],
            sha256=json.loads(Path(str(base) + ".json").read_text())["sha256"]))
    else:
        print("Runtime inputs unchanged: reusing the base; building Linux services/UI only.", flush=True)
        expected = json.loads(Path(plan["base_marker"]).read_text())["sha256"]
        require(digest(base) == expected, "cached base digest changed; use --refresh-base")

    if plan["artifact_engine"] == "apple":
        inspected = run(["container", "image", "inspect", plan["sdk_image"]], capture=True)
        if inspected.returncode:
            # Build an SDK once per Dockerfile/architecture. No application compile.
            with image_builder(run) as limits:
                container_build(run, ["container", "build", *limits,
                                "--arch", plan["arch"], "--progress", "plain",
                                "--tag", plan["sdk_image"], "--file",
                                "containers/sdk/Dockerfile", "containers/sdk"], cwd=source)
    artifact = Path(plan["artifacts"])
    if not artifact.exists():
        command = [sys.executable, HERE / "build-container-artifacts.py", "--source", source,
                   "--cache", plan["cache"], "--output", artifact,
                   "--engine", plan["artifact_engine"], "--arch", plan["arch"],
                   "--cpus", str(plan["compile_cpus"]), "--jobs", str(plan["cargo_jobs"]),
                   "--revision", plan["source_revision"]]
        if plan["artifact_engine"] == "apple":
            command += ["--sdk-image", plan["sdk_image"], "--memory", plan["compile_memory"]]
        run(command)
    require(Path(str(artifact) + ".json").is_file(), "artifact receipt missing; refusing an unverified cached bundle")
    receipt = json.loads(Path(str(artifact) + ".json").read_text())
    require(receipt.get("source_revision") == plan["source_revision"], "artifact receipt source revision mismatch")
    run([*oci, "package", "--base", base, "--artifacts", artifact, "--arch", plan["arch"],
         "--tag", tag, "--output", output])
    run([*oci, "verify", output, "--arch", plan["arch"]])
    run(["container", "image", "load", "--input", output])
    smoke(run, tag, plan["arch"])
    result = dict(plan, tag=tag, archive=str(output), image_loaded=True,
                  checks=["OCI digests/platform", "non-root execution", "service linkage", "UI index",
                          "Python imports", "Linux skill executable inventory"])
    write_json(release / "result.json", result)
    print(json.dumps(result, indent=2), flush=True)
    print("Image prepared and imported. Application boot, host/browser integration and deployment remain separate.", flush=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", default="HEAD", help="committed source revision; working-tree edits are never copied")
    parser.add_argument("--work-dir", type=Path, default=Path("/Volumes/SSD1/magician/image-preparation"))
    parser.add_argument("--arch", choices=("arm64", "amd64"), default="arm64")
    parser.add_argument("--artifact-engine", choices=("zig", "apple"), default="zig",
                        help="application compiler; Zig runs directly on this Mac (default)")
    parser.add_argument("--adopt-base-image",
                        help="replace the cached base with this explicitly reviewed local runtime image")
    parser.add_argument("--initial-base-image",
                        help="adopt this reviewed local runtime image only when no cached base exists")
    parser.add_argument("--force-rebuild-from-scratch", action="store_true",
                        help="ignore cached bases, rebuild runtime dependencies, then compile app artifacts on the host")
    parser.add_argument("--refresh-base", action="store_true", help="rebuild the base even when recorded inputs match")
    parser.add_argument("--dry-run", action="store_true", help="print source/cache/build plan; no writes or runtime calls")
    args = parser.parse_args(argv)
    require(not (args.adopt_base_image and args.initial_base_image),
            "choose either --adopt-base-image or --initial-base-image")
    require(not (args.force_rebuild_from_scratch and args.adopt_base_image),
            "--force-rebuild-from-scratch cannot adopt a base image")
    repo = HERE.parent
    work = args.work_dir.expanduser().resolve()
    require(not work.is_relative_to(repo), "build snapshots/caches must be outside the active checkout")
    require("," not in str(work) and "," not in str(HERE), "Apple bind-mount paths cannot contain commas")
    plan = make_plan(repo, work, args.revision, args.arch, args.refresh_base,
                     args.artifact_engine, args.adopt_base_image, args.initial_base_image,
                     args.force_rebuild_from_scratch)
    dirty = bool(git(repo, "status", "--porcelain", "--untracked-files=no"))
    plan["working_tree_changes_excluded"] = dirty
    print(json.dumps(plan, indent=2), flush=True)
    if dirty:
        print("Working-tree edits are excluded. Commit reviewed changes before preparing their image.", flush=True)
    if args.dry_run:
        return
    require(sys.platform == "darwin", "this entry point targets Apple Container on macOS")
    require(Path("/Volumes/SSD1").is_mount() or args.work_dir != parser.get_default("work_dir"),
            "mount SSD1 before using the default work directory")
    if Path("/Volumes/SSD1").is_mount():
        require(work.is_relative_to(Path("/Volumes/SSD1").resolve()), "use SSD1 for image preparation on this Mac")
    require(shutil.which("container"), "Apple Container CLI is not installed")
    require(platform.machine() == "arm64" or args.arch == "amd64", "requested architecture is not supported on this host")
    work.mkdir(parents=True, exist_ok=True)
    require(shutil.disk_usage(work).free >= 20 * GIB, "image preparation needs at least 20 GiB free (cold builds may need more)")
    with (work / ".prepare.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        # Re-read cache metadata after taking the lock.
        plan = make_plan(repo, work, plan["source_revision"], args.arch, args.refresh_base,
                         args.artifact_engine, args.adopt_base_image, args.initial_base_image,
                         args.force_rebuild_from_scratch)
        release = work / "releases" / (plan["source_revision"][:12] + "-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ"))
        release.mkdir(parents=True)
        (release / "plan.json").write_text(json.dumps(plan, indent=2) + "\n")
        execute(plan, repo, release, Runner(release))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, RuntimeError, OSError, KeyError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"prepare-container-image: {error}", file=sys.stderr)
        raise SystemExit(1)

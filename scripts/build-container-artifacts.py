#!/usr/bin/env python3
"""Build reusable Linux binaries/UI in an explicit isolated source checkout.

Zig mode cross-compiles on macOS without a compiler container. Apple/Docker
mode uses an already built SDK, two CPUs and persistent caches. Native mode is
for a Linux build machine. This never starts an image builder.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import uuid


def require(condition, message):
    if not condition:
        raise ValueError(message)


def run(argv, **kwargs):
    program = shutil.which(str(argv[0]))
    require(program, f"missing program: {argv[0]}")
    return subprocess.run([program, *map(str, argv[1:])], check=True, **kwargs)


def command_output(argv):
    program = shutil.which(str(argv[0]))
    require(program, f"missing program: {argv[0]}")
    return subprocess.check_output([program, *map(str, argv[1:])], text=True).strip()


def zig_target(arch, glibc):
    rust = {"arm64": "aarch64-unknown-linux-gnu", "amd64": "x86_64-unknown-linux-gnu"}[arch]
    return rust, f"{rust}.{glibc}"


def zig_compiler_tmp(cache):
    """Return a short, stable temp path on the cache's filesystem."""
    configured = os.environ.get("MAGICIAN_CONTAINER_BUILD_TMP_ROOT")
    root = Path(configured).expanduser().resolve() if configured else cache.parent.parent / "tmp"
    key = hashlib.sha256(str(cache).encode()).hexdigest()[:12]
    return root / f"zig-{key}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="isolated source snapshot; generated dependencies are written here")
    parser.add_argument("--cache", type=Path, required=True, help="persistent cache for this source/toolchain/architecture")
    parser.add_argument("--output", type=Path, required=True, help="new artifact tar, not an image")
    parser.add_argument("--engine", choices=("apple", "docker", "native", "zig"), default="zig" if sys.platform == "darwin" else "native")
    parser.add_argument("--sdk-image", default="magician-build-sdk:rust1.92-bookworm")
    parser.add_argument("--arch", choices=("arm64", "amd64"), default="arm64" if platform.machine() in ("arm64", "aarch64") else "amd64")
    parser.add_argument("--cpus", type=int, default=2)
    parser.add_argument("--memory", default="12g", help="compile VM limit, independent of runtime's 4g")
    parser.add_argument("--jobs", type=int, help="Cargo/native compile jobs (default: 2 for Zig, 1 otherwise)")
    parser.add_argument("--glibc", default="2.36", help="minimum glibc for Zig output; Debian Bookworm is 2.36")
    parser.add_argument("--revision", help="artifact source identity (use a patch/snapshot identity for dirty sources)")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    source, cache, output = (p.expanduser().resolve() for p in (args.source, args.cache, args.output))
    scripts = Path(__file__).resolve().parent
    require(source != scripts.parent, "use a separate source snapshot; refusing the active working tree")
    require((source / "Cargo.lock").is_file() and (source / "Makefile").is_file(), "source must be a complete Magician source snapshot")
    require(not cache.is_relative_to(source) and not source.is_relative_to(cache), "source and cache must be separate directories")
    require(not output.is_relative_to(source), "output must be outside the source checkout")
    require(not output.exists() and not Path(str(output) + ".json").exists(), "artifact output/receipt already exists")
    jobs = args.jobs if args.jobs is not None else (2 if args.engine == "zig" else 1)
    require(1 <= jobs <= args.cpus <= 4, "require 1 <= jobs <= cpus <= 4; defaults are jobs=2 for Zig, jobs=1 otherwise, cpus=2")
    require(args.engine != "zig" or re.fullmatch(r"[0-9]+\.[0-9]+", args.glibc),
            "--glibc must be a major.minor version")
    native_arch = "arm64" if platform.machine() in ("arm64", "aarch64") else "amd64"
    rust_target, versioned_zig_target = zig_target(args.arch, args.glibc)
    compiler_tmp = zig_compiler_tmp(cache) if args.engine == "zig" else None
    # Deliberately no implicit git HEAD provenance: archives/patches can differ.
    plan = dict(engine=args.engine, platform="linux/" + args.arch, source=str(source), cache=str(cache),
                output=str(output), sdk_image=args.sdk_image if args.engine in ("apple", "docker") else None,
                cpus=args.cpus, memory=args.memory if args.engine in ("apple", "docker") else None,
                cargo_jobs=jobs, source_revision=args.revision,
                rust_target=rust_target if args.engine == "zig" else None,
                zig_target=versioned_zig_target if args.engine == "zig" else None,
                compiler_tmp=str(compiler_tmp) if compiler_tmp else None)
    print(json.dumps(plan, indent=2), flush=True)
    if args.dry_run:
        return
    require(args.engine != "native" or sys.platform.startswith("linux"), "native builds require Linux; macOS binaries are not usable")
    require(args.engine != "zig" or sys.platform == "darwin", "Zig mode currently targets the supported macOS host flow")
    require(args.engine != "native" or args.arch == native_arch, "native build architecture must match the Linux host")
    cache.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    # Nonblocking, cache AND source locks protect concurrent invocations.
    with (cache / ".build.lock").open("a") as cache_lock, (source / ".magician-linux-build.lock").open("a") as source_lock:
        for lock in (cache_lock, source_lock):
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.engine == "zig":
            toolchain = dict(
                engine="zig",
                target=versioned_zig_target,
                zig=command_output(["zig", "version"]),
                cargo_zigbuild=command_output(["cargo-zigbuild", "--version"]),
                rustc=command_output(["rustc", "--version"]),
            )
        else:
            toolchain = dict(engine=args.engine,
                             sdk=args.sdk_image if args.engine in ("apple", "docker") else "native")
        identity = dict(arch=args.arch, toolchain=toolchain)
        identity_path = cache / "identity.json"
        if identity_path.exists():
            require(json.loads(identity_path.read_text()) == identity, "cache architecture/toolchain differs; choose another cache")
        else:
            identity_path.write_text(json.dumps(identity) + "\n")
        # Failed builds keep Cargo/npm caches, but not incomplete output bundles.
        with tempfile.TemporaryDirectory(prefix="payload-", dir=cache) as stage:
            stage = Path(stage)
            if args.engine == "native":
                env = dict(os.environ, CARGO_BUILD_JOBS=str(jobs), CARGO_PROFILE_RELEASE_LTO="false")
                run(["bash", scripts / "build-container-artifacts-linux.sh", source, cache, stage], env=env)
            elif args.engine == "zig":
                compiler_tmp.mkdir(parents=True, exist_ok=True)
                env = dict(os.environ, CARGO_BUILD_JOBS=str(jobs), CARGO_PROFILE_RELEASE_LTO="false",
                           MAGICIAN_CONTAINER_BUILD_TMPDIR=str(compiler_tmp))
                run(["bash", scripts / "build-container-artifacts-zig.sh", source, cache, stage,
                     rust_target, versioned_zig_target, str(jobs)], env=env)
            else:
                engine = "container" if args.engine == "apple" else "docker"
                argv = [engine, "run", "--rm", "--name", "magician-compile-" + uuid.uuid4().hex[:10],
                        "--cpus", str(args.cpus), "--memory", args.memory]
                argv += ["--arch", args.arch, "--progress", "plain"] if engine == "container" else ["--platform", "linux/" + args.arch]
                # Bind only the explicit snapshot, cache, and checked-in helper scripts.
                # Preserve Linux host ownership; Apple virtiofs maps to its host owner.
                if engine == "docker":
                    argv += ["--user", f"{os.getuid()}:{os.getgid()}"]
                for host, guest, readonly in ((source, "/src", False), (cache, "/cache", False), (scripts, "/pipeline", True)):
                    require("," not in str(host), "mount paths cannot contain commas")
                    argv += ["--mount", f"type=bind,source={host},target={guest}" + (",readonly" if readonly else "")]
                argv += ["--env", f"CARGO_BUILD_JOBS={jobs}", "--env", "CARGO_PROFILE_RELEASE_LTO=false",
                         "--entrypoint", "/bin/bash", args.sdk_image, "/pipeline/build-container-artifacts-linux.sh",
                         "/src", "/cache", "/cache/" + stage.name]
                run(argv)
            argv = [sys.executable, scripts / "container-oci.py", "bundle", "--arch", args.arch,
                    "--binaries", stage / "binaries", "--ui", stage / "ui",
                    "--scripts", source / "scripts", "--skills", source / "skillshub",
                    "--output", output]
            if args.revision:
                argv += ["--revision", args.revision]
            run(argv)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"build-container-artifacts: {error}", file=sys.stderr)
        raise SystemExit(1)

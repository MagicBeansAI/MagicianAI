#!/usr/bin/env python3
"""Portable Cargo routing, managed target preparation and native-cache fallback."""
from __future__ import annotations

from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
SHIMS = ROOT / "scripts/build-tools"
sys.path.insert(0, str(ROOT / "scripts"))
from build_cache_defaults import VERSION, storage_root
COMPILE_COMMANDS = {"build", "b", "check", "c", "clippy", "test", "t", "run", "r",
                    "rustc", "rustdoc", "doc", "nextest", "llvm-cov", "llvm-lines", "tauri"}


def clean_path(env):
    return os.pathsep.join(part for part in env.get("PATH", "").split(os.pathsep)
                           if Path(part).resolve() != SHIMS.resolve())


def real_cargo(env):
    configured = env.get("MAGICIAN_REAL_CARGO")
    cargo = configured or shutil.which("cargo", path=clean_path(env))
    if not cargo or Path(cargo).resolve() == (SHIMS / "cargo").resolve():
        raise RuntimeError("Real Cargo not found; install Rust or set MAGICIAN_REAL_CARGO")
    return os.path.abspath(cargo)


def find_mbx(env, root=None, version=None):
    root = Path(root or env.get("MAGICIAN_BUILD_STORAGE_ROOT") or storage_root())
    installed = root / "tools/mbx" / (version or env.get("MBX_VERSION") or VERSION) / "bin/mbx"
    if installed.is_file() and os.access(installed, os.X_OK):
        return str(installed.absolute())
    return shutil.which("mbx", path=clean_path(env))


def mbx_binary(env, bootstrap=False):
    mode = env.get("MAGICIAN_BUILD_CACHE", "auto")
    if mode not in {"auto", "mbx", "off", "0"}:
        raise RuntimeError("MAGICIAN_BUILD_CACHE must be auto, mbx or off")
    if mode in {"off", "0"}:
        return None
    mbx = env.get("MBX_BIN") or find_mbx(env)
    if mbx and os.access(mbx, os.X_OK):
        return os.path.abspath(mbx)
    if bootstrap and env.get("MAGICIAN_BUILD_CACHE_BOOTSTRAP") == "1":
        root = Path(env.get("MAGICIAN_BUILD_STORAGE_ROOT") or storage_root())
        print("build-cache: installing the pinned mr-boxington release for this host", file=sys.stderr)
        subprocess.run([sys.executable, str(ROOT / "scripts/setup-build-cache.py"),
                        "--version", env.get("MBX_VERSION") or VERSION,
                        "--storage-root", str(root)], env=env, check=True, stdout=sys.stderr)
        return find_mbx(env)
    if mode == "mbx":
        raise RuntimeError("Configured mr-boxington is unavailable; run make setup-build-cache "
                           "or explicitly select MAGICIAN_BUILD_CACHE=off")
    return None


def command_name(args):
    index = 0
    while index < len(args):
        arg = args[index]
        if arg.startswith("+"):
            index += 1
        elif arg in {"--config", "-Z", "-C"}:
            index += 2
        elif arg.startswith(("--config=", "-Z", "--color=")) or arg in {"-v", "-vv", "-q", "--verbose", "--quiet", "--offline", "--locked", "--frozen"}:
            index += 1
        else:
            return arg if not arg.startswith("-") else None
    return None


def ensure_storage(env):
    required = env.get("MAGICIAN_BUILD_REQUIRED_VOLUME", "")
    if required and (not Path(required).is_mount() or not os.access(required, os.W_OK)):
        raise RuntimeError(f"Required build volume {required} is not mounted and writable; build stopped")
    temporary = env.get("MAGICIAN_BUILD_TMPDIR") or env.get("TMPDIR")
    if temporary:
        Path(temporary).mkdir(parents=True, exist_ok=True)
        env["TMPDIR"] = temporary


def mbx_environment(env, args=None):
    env = env.copy()
    # Make exports optional settings as empty strings on unconfigured hosts.
    # Treat those as unset so mbx can load its platform/global defaults.
    for key in list(env):
        if key.startswith("MBX_") and env[key] == "":
            env.pop(key)
    env["CARGO"] = real_cargo(env)
    env["PATH"] = clean_path(env)
    env["RUSTC_JOB_GATE_NEXT"] = ""  # mbx owns caching; the gate only limits compilers.
    cache = env.get("SCCACHE")
    if cache and env.get("RUSTC_WRAPPER") == cache:
        env["RUSTC_WRAPPER"] = ""
    gate = env.get("RUSTC_JOB_GATE")
    if args is not None and gate and Path(gate).is_file() and env.get("RUSTC_WRAPPER") == gate:
        # mbx deliberately bypasses Rust caching with an inherited wrapper.
        # Put our gate underneath mbx as its compiler, not around its cache.
        rustup = shutil.which("rustup", path=clean_path(env))
        if not rustup:
            raise RuntimeError("rustup is required to resolve the gated compiler")
        toolchain = args[0][1:] if args and args[0].startswith("+") else env.get("RUSTUP_TOOLCHAIN")
        query = [rustup, "which"] + (["--toolchain", toolchain] if toolchain else [])
        compiler = subprocess.check_output([*query, "rustc"], env=env, text=True, timeout=30).strip()
        env["MAGICIAN_REAL_RUSTC"] = compiler
        env["RUSTC"] = env.get("MAGICIAN_GATED_RUSTC") or str(SHIMS / "rustc-gated")
        if not env.get("RUSTDOC"):
            env["RUSTDOC"] = subprocess.check_output([*query, "rustdoc"], env=env, text=True, timeout=30).strip()
        env["RUSTC_WRAPPER"] = ""
    env.setdefault("MBX_DISPLAY", "plain")
    env.setdefault("MBX_SAVINGS", "plain")
    return env


def target_root(mbx, env):
    result = subprocess.run([mbx, "settings", "get", "target.root"], env=env,
                            check=True, capture_output=True, text=True, timeout=30)
    return Path(result.stdout.strip().strip('"')).expanduser().absolute()


def is_primary_workspace(args, cargo, env):
    manifest = None
    for index, arg in enumerate(args):
        if arg == "--manifest-path" and index + 1 < len(args):
            manifest = args[index + 1]
        elif arg.startswith("--manifest-path="):
            manifest = arg.split("=", 1)[1]
    prefix = [args[0]] if args and args[0].startswith("+") else []
    query = [cargo, *prefix, "locate-project", "--workspace", "--message-format", "plain"]
    if manifest:
        query.extend(["--manifest-path", manifest])
    result = subprocess.run(query, env=env, capture_output=True, text=True, timeout=30)
    workspace = env.get("MAGICIAN_CARGO_WORKSPACE", str(ROOT))
    return result.returncode == 0 and Path(result.stdout.strip()).parent.resolve() == Path(workspace).resolve()


def prepare_target(workspace, managed_root):
    """Retain old outputs; mbx will place and record its own view on this build."""
    workspace = Path(workspace)
    target = workspace / "target"
    local = workspace / ".local"
    local.mkdir(exist_ok=True)
    with (local / "build-cache.lock").open("a+") as operation, ExitStack() as locks:
        fcntl.flock(operation, fcntl.LOCK_EX)
        if target.is_symlink():
            destination = target.resolve()
            if destination.is_relative_to(managed_root.resolve()):
                return
        elif not target.exists():
            return
        else:
            parent = next(path for path in managed_root.parents if path.exists())
            if target.stat().st_dev == parent.stat().st_dev:
                # mbx's own adoption preserves a real directory on this filesystem.
                return
        # Never change a target path while Cargo holds one of its profile locks.
        if target.is_dir():
            for lock_path in [*target.glob("*/.cargo-lock"), *target.glob("*/*/.cargo-lock")]:
                lock = locks.enter_context(lock_path.open("r+"))
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    raise RuntimeError(f"Cargo is using {target}; retry once the build finishes")
        backup = local / f"target-before-mbx-{time.time_ns()}"
        target.rename(backup)
        print(f"build-cache: retained previous target at {backup}; its outputs were not deleted", file=sys.stderr)
        return backup


def managed_ready(workspace):
    target = Path(workspace) / "target"
    if target.is_symlink():
        resolved = target.resolve()
        if resolved.parent.name == "v1" and resolved.with_suffix(".json").is_file():
            return True  # Already managed: no path transition is needed.
    if target.is_dir():
        for path in [*target.glob("*/.cargo-lock"), *target.glob("*/*/.cargo-lock")]:
            try:
                with path.open("r") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except (BlockingIOError, OSError):
                return False
    return True


def native_environment(env):
    env = env.copy()
    cache = env.get("SCCACHE") or shutil.which("sccache", path=clean_path(env))
    explicit = any(env.get(key) for key in ("CC", "CXX", "HOST_CC", "HOST_CXX", "CC_KNOWN_WRAPPER_CUSTOM"))
    if env.get("MAGICIAN_NATIVE_CACHE", "auto") not in {"off", "0"} and cache and not explicit:
        cc = shutil.which("cc", path=clean_path(env))
        cxx = shutil.which("c++", path=clean_path(env))
        if cc and cxx:
            wrapper = SHIMS / "sccache-cxx"
            if " " not in str(wrapper):
                env.update(SCCACHE=os.path.abspath(cache), HOST_CC=f"{wrapper} {cc}",
                           HOST_CXX=f"{wrapper} {cxx}", CC_KNOWN_WRAPPER_CUSTOM="sccache-cxx")
    env["PATH"] = clean_path(env)
    return env


def cargo_plan(args, env):
    env = env.copy()
    for key in list(env):
        if key.startswith("SCCACHE_") and env[key] == "":
            env.pop(key)
    cargo = real_cargo(env)
    if command_name(args) not in COMPILE_COMMANDS:
        env["PATH"] = clean_path(env)
        return [cargo, *args], env
    ensure_storage(env)
    # Cargo's offline/frozen contract also prevents a first-use tool download.
    offline = any(arg in {"--offline", "--frozen"} for arg in args) or env.get("CARGO_NET_OFFLINE", "").lower() in {"true", "1"}
    mbx = mbx_binary(env, bootstrap=not offline)
    if not mbx:
        view = env.get("MAGICIAN_CARGO_MANAGED_VIEW")
        destination = env.get("MAGICIAN_CARGO_UNMANAGED_TARGET")
        explicit_flag = any(arg == "--target-dir" or arg.startswith("--target-dir=") for arg in args)
        if view and env.get("CARGO_TARGET_DIR") == view and destination and not explicit_flag and not os.path.lexists(view):
            if Path(destination).absolute() != Path(view).absolute():
                Path(destination).mkdir(parents=True, exist_ok=True)
                Path(view).symlink_to(Path(destination).absolute(), target_is_directory=True)
        if env.get("SCCACHE") and env.get("MAGICIAN_BUILD_STORAGE_ROOT"):
            import importlib
            setup = importlib.import_module("setup-build-cache")
            setup.prepare_tools(Path(env["MAGICIAN_BUILD_STORAGE_ROOT"]))
        return [cargo, *args], native_environment(env)
    if env.get("MAGICIAN_BUILD_STORAGE_ROOT"):
        import importlib
        setup = importlib.import_module("setup-build-cache")
        setup.prepare_tools(Path(env["MAGICIAN_BUILD_STORAGE_ROOT"]))
    env = mbx_environment(env, args)
    view = env.get("MAGICIAN_CARGO_MANAGED_VIEW")
    explicit_flag = any(arg == "--target-dir" or arg.startswith("--target-dir=") for arg in args)
    if view and env.get("CARGO_TARGET_DIR") == view and not explicit_flag and is_primary_workspace(args, cargo, env):
        env.pop("CARGO_TARGET_DIR", None)
        backup = prepare_target(env.get("MAGICIAN_CARGO_WORKSPACE", str(ROOT)), target_root(mbx, env))
        if backup:
            env["MAGICIAN_CARGO_TARGET_BACKUP"] = str(backup)
    # Retain the stable dev edit loop from the first build, while honoring an
    # explicit low-disk opt-out for both eager and learned incremental state.
    # Release/explicit profiles otherwise use mbx's normal policy. An explicit
    # mbx setting still wins.
    release = any(arg in {"--release", "-r", "--profile"} or arg.startswith("--profile=") for arg in args)
    if env.get("CARGO_INCREMENTAL") == "0":
        env.setdefault("MBX_LEARNED_INCREMENTAL", "0")
    if not release:
        env.setdefault("MBX_EAGER_INCREMENTAL", "0" if env.get("CARGO_INCREMENTAL") == "0" else "1")
    return [mbx, *args], env


def launch(command, env):
    child = subprocess.Popen(command, env=env, close_fds=False)
    previous = {}
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        previous[sig] = signal.signal(sig, lambda received, _frame: child.send_signal(received))
    try:
        code = child.wait()
        return code if code >= 0 else 128 - code
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        backup = env.get("MAGICIAN_CARGO_TARGET_BACKUP")
        view = env.get("MAGICIAN_CARGO_MANAGED_VIEW")
        if backup and view and not os.path.lexists(view):
            Path(backup).rename(view)
            print("build-cache: restored the previous target link after placement failed", file=sys.stderr)


def main():
    env = os.environ.copy()
    if len(sys.argv) > 1 and sys.argv[1] == "__cargo__":
        command, env = cargo_plan(sys.argv[2:], env)
        raise SystemExit(launch(command, env))
    action = sys.argv[1] if len(sys.argv) > 1 else "status"
    if action == "find-mbx":
        print(find_mbx(env, sys.argv[2], sys.argv[3]) or "")
        return
    if action == "managed-ready":
        print("1" if managed_ready(ROOT) else "0")
        return
    mbx = mbx_binary(env)
    if action == "status":
        print(json.dumps({"backend": "mbx" if mbx else "cargo", "cargo": real_cargo(env),
                          "mbx": mbx, "target": env.get("CARGO_TARGET_DIR"),
                          "bootstrap_on_build": env.get("MAGICIAN_BUILD_CACHE_BOOTSTRAP") == "1" and not mbx and env.get("MAGICIAN_BUILD_CACHE", "auto") == "auto" and env.get("CARGO_NET_OFFLINE", "").lower() not in {"true", "1"},
                          "storage_root": env.get("MAGICIAN_BUILD_STORAGE_ROOT") or None,
                          "auxiliary_storage": env.get("MAGICIAN_BUILD_AUX_DIR") or None,
                          "managed_view": env.get("MAGICIAN_CARGO_MANAGED_VIEW") or None,
                          "management_deferred": env.get("MAGICIAN_CARGO_MANAGED_DEFERRED") == "1",
                          "cache": env.get("MBX_CACHE_DIR") or "platform default",
                          "total_budget": env.get("MBX_GC_MAX_TOTAL_SIZE") or "mbx default",
                          "minimum_free": env.get("MBX_GC_MIN_FREE_SIZE") or "mbx default",
                          "required_volume": env.get("MAGICIAN_BUILD_REQUIRED_VOLUME") or None,
                          "rustc_wrapper": env.get("RUSTC_WRAPPER"), "native_fallback": env.get("SCCACHE")}, indent=2))
    elif action == "link-target":
        ensure_storage(env)
        target = ROOT / "target"
        if not target.exists() and not target.is_symlink():
            destination = Path(env.get("CARGO_TARGET_DIR_BASE") or env.get("MAGICIAN_CARGO_UNMANAGED_TARGET")
                               or env.get("CARGO_TARGET_ROOT", str(target))).absolute()
            destination.mkdir(parents=True, exist_ok=True)
            if destination != target:
                target.symlink_to(destination, target_is_directory=True)
        print(f"target -> {target.resolve()}" + ("; mbx manages it on the next build" if mbx else ""))
    elif action == "maintain":
        ensure_storage(env)
        if mbx and env.get("MAGICIAN_CARGO_MANAGED_VIEW"):
            subprocess.run([mbx, "gc", "--automatic"], env=mbx_environment(env), check=True)
        else:
            subprocess.run([sys.executable, str(ROOT / "scripts/maintain-build-cache.py"),
                            "--target-dir", env.get("CARGO_TARGET_DIR", str(ROOT / "target"))], check=True)
    elif action in {"doctor", "gc-preview", "stats", "analyze"}:
        if not mbx:
            raise RuntimeError("mr-boxington is not installed; run make setup-build-cache")
        ensure_storage(env)
        args = {"doctor": ["doctor"], "gc-preview": ["gc", "--dry-run"],
                "stats": ["cache", "stats"], "analyze": ["analyze"]}[action]
        subprocess.run([mbx, *args], env=mbx_environment(env), check=True)
    else:
        raise RuntimeError(f"Unknown build-cache action: {action}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        raise SystemExit(f"build-cache: {error}")

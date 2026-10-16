#!/usr/bin/env python3
"""Seed a source runtime and manage the macOS per-user native backend service."""
from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

SERVICE_ID = "ai.magicbeans.magican.backend"


def absolute_path(value: str) -> Path:
    path = Path(value).expanduser()
    if not path.is_absolute():
        raise ValueError("installation paths must be absolute")
    return path.resolve()


def validate_paths(prefix: Path, data_root: Path) -> None:
    prefix, data_root = prefix.resolve(), data_root.resolve()
    if prefix == data_root or prefix in data_root.parents or data_root in prefix.parents:
        raise ValueError("runtime data and installed software must use separate, non-overlapping directories")


def copy_absent(source: Path, destination: Path, private: bool = False) -> None:
    if destination.exists():
        return
    if not source.is_file():
        raise ValueError(f"missing runtime seed: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Write completely before making the seed visible. Linking without replace
    # keeps concurrent installers and interrupted copies from damaging owner files.
    staging = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output, source.open("rb") as input_file:
            staging = Path(output.name)
            os.chmod(staging, 0o600 if private else 0o644)
            shutil.copyfileobj(input_file, output)
        os.link(staging, destination)
    except FileExistsError:
        return
    finally:
        if staging:
            staging.unlink(missing_ok=True)


def seed(checkout: Path, data_root: Path) -> None:
    for name in ("magician-config.yaml", "llm-router.yaml", "decision-engine.yaml"):
        copy_absent(checkout / name, data_root / name)
    seeds = checkout / "magician_data_v3"
    for name in (".env", ".env.development"):
        copy_absent(seeds / ".env.example", data_root / name, private=True)
    copy_absent(seeds / "operator-config.template.yaml", data_root / "operator-config.yaml", private=True)
    for name in ("Inbox", "Space", "secrets", "logs"):
        (data_root / name).mkdir(parents=True, exist_ok=True)
    scoped = Path("scopes/anonymous/default")
    files = [scoped / "programs/harness_reliability.md"]
    files.extend(scoped / f"agent_runtime/agents/{agent}/definition.agent.yaml"
                 for agent in ("harness-sre", "cto", "internal-system-analyst"))
    for relative in files:
        copy_absent(seeds / relative, data_root / relative)


def service_definition(prefix: Path, data_root: Path) -> dict:
    revision = hashlib.sha256()
    for name in ("MANIFEST.yaml", "magician.bin", "magicutor.bin", "magic-supervisor.bin"):
        file = prefix / name
        if file.is_file():
            stat = file.stat()
            revision.update(f"{name}:{stat.st_size}:{stat.st_mtime_ns}".encode())
    for name in (".env", ".env.development", "magician-config.yaml", "llm-router.yaml", "decision-engine.yaml"):
        file = data_root / name
        if file.is_file():
            stat = file.stat()
            revision.update(f"{name}:{stat.st_size}:{stat.st_mtime_ns}".encode())
    paths = [str(prefix), str(Path.home() / ".local/bin"), str(Path.home() / ".cargo/bin"),
             "/opt/homebrew/bin", "/usr/local/bin"]
    paths.extend(os.environ.get("PATH", "").split(os.pathsep))
    paths.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"])
    paths = list(dict.fromkeys(path for path in paths if path and "\n" not in path and "\r" not in path))
    log = str(data_root / "logs/native-supervisor.log")
    environment = {
        "MAGICIAN_ROOT_DIR": str(data_root),
        "MAGICIAN_HOST_GATEWAY_URL": "http://127.0.0.1:3017",
        "MAGICIAN_INSTALL_REVISION": revision.hexdigest(),
        "PATH": os.pathsep.join(paths),
    }
    if (prefix / "share/ui/index.html").is_file():
        environment["MAGICIAN_FRONTEND_DIR"] = str(prefix / "share/ui")
    return {
        "Label": SERVICE_ID,
        "ProgramArguments": ["/bin/bash", str(prefix / "scripts/run-native-backend.sh")],
        "WorkingDirectory": str(prefix),
        "EnvironmentVariables": environment,
        "RunAtLoad": True,
        "KeepAlive": True,
        "ProcessType": "Background",
        "SoftResourceLimits": {"NumberOfFiles": 8192},
        "StandardOutPath": log,
        "StandardErrorPath": log,
    }


def check_service_ports(runner, target: str, *, require_bound: bool = False) -> None:
    registration = runner(["/bin/launchctl", "print", target], capture_output=True, text=True)
    match = re.search(r"^\s*pid = (\d+)\s*$", registration.stdout or "", re.MULTILINE)
    owned = {int(match.group(1))} if registration.returncode == 0 and match else set()
    if owned:
        processes = runner(["/bin/ps", "-axo", "pid,ppid"], capture_output=True, text=True)
        relationships = [tuple(map(int, line.split())) for line in processes.stdout.splitlines()
                         if len(line.split()) == 2 and all(part.isdigit() for part in line.split())]
        while True:
            descendants = {pid for pid, parent in relationships if parent in owned}
            if descendants <= owned:
                break
            owned.update(descendants)
    for port in (8081, 3002, 3003):
        result = runner(["/usr/sbin/lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"], capture_output=True, text=True)
        listeners = {int(pid) for pid in result.stdout.split() if pid.isdigit()}
        if require_bound and not listeners:
            raise ValueError(f"native service has not bound port {port} yet")
        if listeners - owned:
            raise ValueError(f"port {port} is used by another backend or application; stop that service before installing the native backend")


def check_desktop_gateway(*, runner=subprocess.run) -> None:
    result = runner(["/usr/sbin/lsof", "-nP", "-iTCP:3017", "-sTCP:LISTEN", "-t"], capture_output=True, text=True)
    if result.returncode not in (0, 1):
        raise ValueError("could not check the Desktop gateway before restarting Magican")
    for pid in result.stdout.split():
        if not pid.isdigit():
            continue
        process = runner(["/bin/ps", "-p", pid, "-o", "comm="], capture_output=True, text=True)
        executable = Path(process.stdout.strip())
        if process.returncode or executable.name not in ("magician-desktop", "magician-desktop.bin"):
            raise ValueError("port 3017 is used by another application; stop it before starting Magican Desktop")


def verify_binding(prefix: Path, data_root: Path, *, runner=subprocess.run, service_path: Path | None = None) -> None:
    destination = service_path or Path.home() / "Library/LaunchAgents" / f"{SERVICE_ID}.plist"
    if not destination.is_file():
        raise ValueError("the native backend service is not registered")
    actual = plistlib.loads(destination.read_bytes())
    expected = service_definition(prefix, data_root)
    actual_env = actual.get("EnvironmentVariables", {})
    expected_env = expected["EnvironmentVariables"]
    if actual.get("ProgramArguments") != expected["ProgramArguments"] or any(
        actual_env.get(key) != expected_env[key] for key in ("MAGICIAN_ROOT_DIR", "MAGICIAN_INSTALL_REVISION")
    ):
        raise ValueError("native service is bound to a different installation, data folder or runtime revision")
    check_service_ports(runner, f"gui/{os.geteuid()}/{SERVICE_ID}", require_bound=True)


def start(prefix: Path, data_root: Path, *, runner=subprocess.run, service_path: Path | None = None) -> None:
    validate_paths(prefix.resolve(), data_root.resolve())
    for name in ("magician.bin", "magicutor.bin", "magic-supervisor.bin", "MANIFEST.yaml", "tool-runtime-config.yaml", "scripts/run-native-backend.sh", "scripts/run-ollama.sh"):
        if not (prefix / name).is_file():
            raise ValueError(f"native installation is incomplete: {prefix / name}")
    for name in ("magician-config.yaml", "llm-router.yaml", "decision-engine.yaml"):
        if not (data_root / name).is_file():
            raise ValueError(f"runtime configuration is missing: {data_root / name}")
    destination = service_path or Path.home() / "Library/LaunchAgents" / f"{SERVICE_ID}.plist"
    definition = plistlib.dumps(service_definition(prefix, data_root), sort_keys=False)
    domain = f"gui/{os.geteuid()}"
    target = f"{domain}/{SERVICE_ID}"
    check_service_ports(runner, target)
    existing = destination.read_bytes() if destination.is_file() else None
    # A re-run with the same prefix and data root keeps the registered binding.
    if existing == definition:
        result = runner(["/bin/launchctl", "kickstart", target], capture_output=True, text=True)
        if result.returncode == 0:
            return
    destination.parent.mkdir(parents=True, exist_ok=True)
    (data_root / "logs").mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as temporary:
        temporary.write(definition)
        staging = Path(temporary.name)
    try:
        os.replace(staging, destination)
    finally:
        staging.unlink(missing_ok=True)
    runner(["/bin/launchctl", "bootout", target], capture_output=True, text=True)
    for command in (["enable", target], ["bootstrap", domain, str(destination)], ["kickstart", target]):
        result = runner(["/bin/launchctl", *command], capture_output=True, text=True)
        if result.returncode:
            raise ValueError(f"native service {command[0]} failed: {result.stderr.strip() or result.stdout.strip()}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("validate", "seed", "start", "verify", "check-desktop"))
    parser.add_argument("--checkout")
    parser.add_argument("--prefix")
    parser.add_argument("--data-dir", required=True)
    args = parser.parse_args()
    try:
        data_root = absolute_path(args.data_dir)
        if args.prefix:
            validate_paths(absolute_path(args.prefix), data_root)
        if args.action == "validate":
            if not args.prefix:
                raise ValueError("validate needs --prefix")
            return 0
        if args.action == "seed":
            if not args.checkout:
                raise ValueError("seed needs --checkout")
            seed(absolute_path(args.checkout), data_root)
        else:
            if sys.platform != "darwin":
                raise ValueError("native source services are supported on macOS")
            if args.action == "check-desktop":
                check_desktop_gateway()
                return 0
            if not args.prefix:
                raise ValueError("start needs --prefix")
            if args.action == "start":
                start(absolute_path(args.prefix), data_root)
            else:
                verify_binding(absolute_path(args.prefix), data_root)
    except (OSError, ValueError) as error:
        print(f"Native installation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

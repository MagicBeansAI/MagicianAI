#!/usr/bin/env python3
"""Seed a source runtime and manage the macOS per-user native backend service."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import xml.parsers.expat

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
    # Only `.env`: both env loaders read `.env.development` first and never
    # overwrite, so the example's empty `KEY=` lines would shadow `.env`.
    copy_absent(seeds / ".env.example", data_root / ".env", private=True)
    copy_absent(seeds / "operator-config.template.yaml", data_root / "operator-config.yaml", private=True)
    for name in ("Inbox", "Space", "secrets", "logs"):
        (data_root / name).mkdir(parents=True, exist_ok=True)
    scoped = Path("scopes/anonymous/default")
    files = [scoped / "programs/harness_reliability.md"]
    files.extend(scoped / f"agent_runtime/agents/{agent}/definition.agent.yaml"
                 for agent in ("harness-sre", "cto", "internal-system-analyst"))
    for relative in files:
        copy_absent(seeds / relative, data_root / relative)


def launcher_arguments(prefix: Path) -> list[str]:
    """The fixed part of ProgramArguments: which installation runs."""
    return ["/bin/bash", str(prefix / "scripts/run-native-backend.sh")]


def service_definition(prefix: Path, data_root: Path, supervisor_args: list[str] | None = None) -> dict:
    """`supervisor_args` (from Magican Desktop) are appended to ProgramArguments
    and forwarded by run-native-backend.sh to magic-supervisor: the runtime
    config path and the desktop bundle/log for its crash-only relaunch."""
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
        "ProgramArguments": launcher_arguments(prefix) + list(supervisor_args or []),
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
    # Only the launcher identifies the installation; the supervisor arguments
    # after it (desktop bundle, config path) may differ between app versions.
    if actual.get("ProgramArguments", [])[:2] != expected["ProgramArguments"][:2] or any(
        actual_env.get(key) != expected_env[key] for key in ("MAGICIAN_ROOT_DIR", "MAGICIAN_INSTALL_REVISION")
    ):
        raise ValueError("native service is bound to a different installation, data folder or runtime revision")
    check_service_ports(runner, f"gui/{os.geteuid()}/{SERVICE_ID}", require_bound=True)


def write_service(destination: Path, definition: bytes) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as temporary:
        temporary.write(definition)
        staging = Path(temporary.name)
    try:
        os.replace(staging, destination)
    finally:
        staging.unlink(missing_ok=True)


def rebootstrap(destination: Path, runner) -> None:
    domain = f"gui/{os.geteuid()}"
    target = f"{domain}/{SERVICE_ID}"
    runner(["/bin/launchctl", "bootout", target], capture_output=True, text=True)
    for command in (["enable", target], ["bootstrap", domain, str(destination)], ["kickstart", target]):
        result = runner(["/bin/launchctl", *command], capture_output=True, text=True)
        if result.returncode:
            raise ValueError(f"native service {command[0]} failed: {result.stderr.strip() or result.stdout.strip()}")


REPLACED_FLAGS = ("--magician-config", "--desktop-app", "--desktop-log")


def split_flags(args: list[str]) -> list[tuple[str, list[str]]]:
    """Group `--flag value`, `--flag=value` and bare `--flag` items."""
    items: list[tuple[str, list[str]]] = []
    index = 0
    while index < len(args):
        token = args[index]
        flag = token.split("=", 1)[0]
        if token.startswith("--") and "=" not in token and index + 1 < len(args) \
                and not args[index + 1].startswith("--"):
            items.append((flag, [token, args[index + 1]]))
            index += 2
        else:
            items.append((flag, [token]))
            index += 1
    return items


def is_applications_bundle(bundle: Path) -> bool:
    text = str(bundle)
    return text.startswith("/Applications/") or text.startswith(str(Path.home() / "Applications") + "/")


def merge_supervisor_args(recorded: list[str], candidate: list[str], bundle_exists=Path.is_dir,
                          preferred=is_applications_bundle) -> list[str]:
    """The supervisor arguments to record.

    The config path follows the candidate when it has one. The candidate's
    `--desktop-app` (and log) wins when it runs from /Applications or
    ~/Applications, or when no existing bundle is recorded; otherwise the
    recorded one is kept, so a worktree debug build or a stale copy never
    steals the binding and an installed app always reclaims it. Every other
    recorded flag (a hand-added `--manage-desktop=0`, a future flag) is kept."""
    def value(args: list[str], flag: str):
        return next((args[i + 1] for i in range(len(args) - 1) if args[i] == flag), None)
    config = value(candidate, "--magician-config") or value(recorded, "--magician-config")
    recorded_bundle = value(recorded, "--desktop-app")
    candidate_bundle = value(candidate, "--desktop-app")
    if candidate_bundle is not None and (
            preferred(Path(candidate_bundle)) or recorded_bundle is None
            or not bundle_exists(Path(recorded_bundle))):
        bundle, log = candidate_bundle, value(candidate, "--desktop-log")
    else:
        bundle, log = recorded_bundle, value(recorded, "--desktop-log")
    merged: list[str] = []
    if config is not None:
        merged += ["--magician-config", config]
    if bundle is not None:
        merged += ["--desktop-app", bundle]
        if log is not None:
            merged += ["--desktop-log", log]
    for flag, tokens in split_flags(recorded):
        if flag not in REPLACED_FLAGS:
            merged += tokens
    return merged


def load_registration(destination: Path) -> dict | None:
    """The registered LaunchAgent as a dict, or None when it is missing,
    empty, corrupt, truncated or not a dict with a list ProgramArguments, so
    start rewrites a clean plist and sync leaves it alone."""
    try:
        if not destination.is_file():
            return None
        actual = plistlib.loads(destination.read_bytes())
    except (OSError, ValueError, xml.parsers.expat.ExpatError, plistlib.InvalidFileException):
        return None
    if not isinstance(actual, dict) or not isinstance(actual.get("ProgramArguments", []), list):
        return None
    if not isinstance(actual.get("EnvironmentVariables", {}), dict):
        return None
    return actual


def recorded_arguments(actual: dict | None, prefix: Path, data_root: Path) -> list[str] | None:
    """The supervisor arguments of a registration bound to this installation
    and data folder, else None."""
    if actual is None:
        return None
    if actual.get("EnvironmentVariables", {}).get("MAGICIAN_ROOT_DIR") != str(data_root):
        return None
    current = actual.get("ProgramArguments", [])
    if current[:2] != launcher_arguments(prefix):
        return None
    return [str(item) for item in current[2:]]


def sync_supervisor_args(prefix: Path, data_root: Path, supervisor_args: list[str], *,
                         service_path: Path | None = None, bundle_exists=Path.is_dir,
                         preferred=is_applications_bundle) -> bool:
    """Record current supervisor arguments in an existing registration on
    Desktop launch. Only ProgramArguments are compared and changed (the
    install revision and environment are kept), and the file is rewritten
    without re-bootstrapping: the running backend is never restarted for
    this, and launchd loads the arguments at the next login or
    re-registration. A registration bound to another installation or data
    folder is left alone. Returns whether it was rewritten."""
    destination = service_path or Path.home() / "Library/LaunchAgents" / f"{SERVICE_ID}.plist"
    actual = load_registration(destination)
    if actual is None:
        # Missing or unparseable: nothing to sync; setup rewrites it.
        return False
    recorded = recorded_arguments(actual, prefix, data_root)
    if recorded is None:
        return False
    merged = merge_supervisor_args(recorded, list(supervisor_args), bundle_exists, preferred)
    if merged == recorded:
        return False
    actual["ProgramArguments"] = launcher_arguments(prefix) + merged
    write_service(destination, plistlib.dumps(actual, sort_keys=False))
    return True


def loaded_program_arguments(runner, target: str) -> list[str] | None:
    """The ProgramArguments launchd has loaded for `target` (`launchctl
    print`), which can differ from the plist file after a file-only sync."""
    result = runner(["/bin/launchctl", "print", target], capture_output=True, text=True)
    if result.returncode != 0:
        return None
    arguments: list[str] = []
    inside = False
    for line in (result.stdout or "").splitlines():
        stripped = line.strip()
        if not inside:
            if stripped == "arguments = {":
                inside = True
            continue
        if stripped == "}":
            return arguments
        arguments.append(stripped)
    return None


def start(prefix: Path, data_root: Path, *, runner=subprocess.run, service_path: Path | None = None,
          supervisor_args: list[str] | None = None, bundle_exists=Path.is_dir,
          preferred=is_applications_bundle) -> None:
    validate_paths(prefix.resolve(), data_root.resolve())
    for name in ("magician.bin", "magicutor.bin", "magic-supervisor.bin", "MANIFEST.yaml", "tool-runtime-config.yaml", "scripts/run-native-backend.sh", "scripts/run-ollama.sh"):
        if not (prefix / name).is_file():
            raise ValueError(f"native installation is incomplete: {prefix / name}")
    for name in ("magician-config.yaml", "llm-router.yaml", "decision-engine.yaml"):
        if not (data_root / name).is_file():
            raise ValueError(f"runtime configuration is missing: {data_root / name}")
    destination = service_path or Path.home() / "Library/LaunchAgents" / f"{SERVICE_ID}.plist"
    # Re-running setup (terminal wizard, source installer: no arguments; or
    # Desktop: its own) merges with what this registration already records, so
    # Desktop's arguments are never dropped and an unchanged re-run stays on
    # the kickstart-only path below.
    # A missing or unparseable registration is simply rewritten below.
    recorded = recorded_arguments(load_registration(destination), prefix, data_root)
    if recorded is not None:
        supervisor_args = merge_supervisor_args(recorded, list(supervisor_args or []), bundle_exists, preferred)
    service = service_definition(prefix, data_root, supervisor_args)
    definition = plistlib.dumps(service, sort_keys=False)
    domain = f"gui/{os.geteuid()}"
    target = f"{domain}/{SERVICE_ID}"
    check_service_ports(runner, target)
    existing = destination.read_bytes() if destination.is_file() else None
    # A re-run with the same prefix and data root keeps the registered binding,
    # unless launchd still runs older arguments than the file (a file-only
    # sync): setup is explicit, so it re-registers then.
    if existing == definition:
        loaded = loaded_program_arguments(runner, target)
        if loaded is None or loaded == service["ProgramArguments"]:
            result = runner(["/bin/launchctl", "kickstart", target], capture_output=True, text=True)
            if result.returncode == 0:
                return
    (data_root / "logs").mkdir(parents=True, exist_ok=True)
    write_service(destination, definition)
    rebootstrap(destination, runner)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("validate", "seed", "start", "verify", "check-desktop", "sync-supervisor-args"))
    parser.add_argument("--checkout")
    parser.add_argument("--prefix")
    parser.add_argument("--data-dir", required=True)
    parser.add_argument("--supervisor-args-json",
                        help="JSON list of magic-supervisor arguments to append to the LaunchAgent")
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
            supervisor_args = None
            if args.supervisor_args_json is not None:
                supervisor_args = json.loads(args.supervisor_args_json)
                if not isinstance(supervisor_args, list) or not all(isinstance(item, str) for item in supervisor_args):
                    raise ValueError("--supervisor-args-json must be a JSON list of strings")
            if args.action == "start":
                start(absolute_path(args.prefix), data_root, supervisor_args=supervisor_args)
            elif args.action == "sync-supervisor-args":
                changed = sync_supervisor_args(absolute_path(args.prefix), data_root, supervisor_args or [])
                print("updated" if changed else "unchanged")
            else:
                verify_binding(absolute_path(args.prefix), data_root)
    except (OSError, ValueError) as error:
        print(f"Native installation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

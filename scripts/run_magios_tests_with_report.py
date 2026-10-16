#!/usr/bin/env python3
"""Run Magios unit tests with isolated Xcode state and a fresh report."""

from __future__ import annotations

import argparse
import fcntl
import json
import math
import os
import platform
import pwd
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Callable, Iterator, Sequence


MAX_INFRASTRUCTURE_RETRIES = 1
DEFAULT_LOCK_TIMEOUT_SECONDS = 5.0
DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS = 30.0
DIAGNOSTIC_TERMINATE_GRACE_SECONDS = 2.0
DIAGNOSTIC_POLL_SECONDS = 0.5
FSEVENTS_MARKERS = ("DVTFilePathFSEvents", "Failed to start fs event stream")
CACHE_MARKERS = ("confstr", "DARWIN_USER_CACHE_DIR")
CACHE_IO_MARKERS = ("Input/output error", "I/O error")
XCTEST_ACTIVITY_MARKERS = (
    "Testing started",
    "Testing failed",
    "Test Suite '",
    "Test Case '",
    "Failing tests:",
    "** TEST ",
    "Executed ",
)
FILTERED_DEVICE_NOISE = (
    "notification_proxy",
    "passcode protected",
    "DTDKRemoteDeviceConnection",
    "com.apple.dtdevicekit",
    "MobileDeviceError",
    "DVTRadarComponentKey",
    "DTDeviceKitBase",
    "Please check your connection to your device",
)
INHERITED_HOST_CONTEXT_VARIABLES = (
    "APP_SANDBOX_CONTAINER_ID",
    "COMMAND_MODE",
    "XPC_FLAGS",
    "XPC_SERVICE_NAME",
)
PRESERVED_REAL_HOME_DIRECTORIES = (
    Path("Library/Developer/CoreSimulator/Devices"),
    Path("Library/Developer/Xcode/UserData"),
    Path("Library/MobileDevice"),
)
PRESERVED_REAL_HOME_FILES = (
    Path(".gitconfig"),
    Path("Library/Developer/CoreSimulator/RuntimeMap.plist"),
    Path("Library/Preferences/com.apple.dt.Xcode.plist"),
)


@dataclass(frozen=True)
class AttemptResult:
    number: int
    status: int
    bundle: Path
    stderr_path: Path
    stderr: str
    stdout: str = ""


@dataclass(frozen=True)
class HostContext:
    launch_prefix: tuple[str, ...]
    warnings: tuple[str, ...]
    aqua_available: bool
    darwin_cache_available: bool


class RunnerBusyError(RuntimeError):
    pass


@dataclass
class DiagnosticProcessState:
    first_seen: float
    terminate_sent_at: float | None = None


def shell_status(returncode: int) -> int:
    """Convert subprocess signal return codes to the shell's 128+signal form."""
    return 128 + abs(returncode) if returncode < 0 else returncode


def run_probe(command: Sequence[str]) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(command),
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
        )
    except OSError as error:
        return subprocess.CompletedProcess(list(command), 127, "", str(error))


def parse_process_table(output: str) -> dict[int, tuple[int, str]]:
    """Parse the stable pid/ppid/command columns emitted by BSD ps."""
    processes: dict[int, tuple[int, str]] = {}
    for raw_line in output.splitlines():
        fields = raw_line.strip().split(None, 2)
        if len(fields) != 3:
            continue
        try:
            pid, parent = int(fields[0]), int(fields[1])
        except ValueError:
            continue
        processes[pid] = (parent, fields[2])
    return processes


def descendant_pids(root_pid: int, processes: dict[int, tuple[int, str]]) -> set[int]:
    descendants: set[int] = set()
    frontier = [root_pid]
    while frontier:
        parent = frontier.pop()
        children = [pid for pid, (ppid, _) in processes.items() if ppid == parent]
        for child in children:
            if child in descendants:
                continue
            descendants.add(child)
            frontier.append(child)
    return descendants


def diagnostic_watch_actions(
    *,
    root_pid: int,
    processes: dict[int, tuple[int, str]],
    states: dict[int, DiagnosticProcessState],
    now: float,
    timeout_seconds: float,
    terminate_grace_seconds: float = DIAGNOSTIC_TERMINATE_GRACE_SECONDS,
) -> list[tuple[int, signal.Signals]]:
    """Return bounded signals only for descendant `simctl diagnose` processes."""
    diagnostics = {
        pid
        for pid in descendant_pids(root_pid, processes)
        if "simctl diagnose" in processes[pid][1]
    }
    for pid in tuple(states):
        if pid not in diagnostics:
            states.pop(pid, None)

    actions: list[tuple[int, signal.Signals]] = []
    for pid in sorted(diagnostics):
        state = states.setdefault(pid, DiagnosticProcessState(first_seen=now))
        if state.terminate_sent_at is None:
            if now - state.first_seen >= timeout_seconds:
                state.terminate_sent_at = now
                actions.append((pid, signal.SIGTERM))
        elif now - state.terminate_sent_at >= terminate_grace_seconds:
            actions.append((pid, signal.SIGKILL))
    return actions


def watch_simctl_diagnostics(
    *,
    root_pid: int,
    timeout_seconds: float,
    stop: threading.Event,
    capped_pids: set[int],
) -> None:
    """Cap Xcode's post-test diagnostics without shortening XCTest itself."""
    states: dict[int, DiagnosticProcessState] = {}
    while not stop.wait(DIAGNOSTIC_POLL_SECONDS):
        probe = run_probe(("ps", "-axo", "pid=,ppid=,command="))
        if probe.returncode != 0:
            continue
        processes = parse_process_table(probe.stdout)
        for pid, requested_signal in diagnostic_watch_actions(
            root_pid=root_pid,
            processes=processes,
            states=states,
            now=time.monotonic(),
            timeout_seconds=timeout_seconds,
        ):
            try:
                os.kill(pid, requested_signal)
                capped_pids.add(pid)
            except ProcessLookupError:
                states.pop(pid, None)
            except PermissionError:
                # The runner still remains bounded by Xcode's own timeout. Do not
                # widen authority or terminate the parent test process.
                states.pop(pid, None)


def probe_detail(result: subprocess.CompletedProcess[str]) -> str:
    detail = (result.stderr or result.stdout or "unknown failure").strip()
    return detail.splitlines()[-1] if detail else "unknown failure"


def inspect_host_context(
    *,
    system: str | None = None,
    uid: int | None = None,
    probe: Callable[[Sequence[str]], subprocess.CompletedProcess[str]] = run_probe,
) -> HostContext:
    """Classify Darwin cache resolution and the user bootstrap used by Xcode."""
    if (system or platform.system()) != "Darwin":
        return HostContext((), (), True, True)

    resolved_uid = os.getuid() if uid is None else uid
    warnings: list[str] = []
    cache_probe = probe(("getconf", "DARWIN_USER_CACHE_DIR"))
    cache_available = cache_probe.returncode == 0 and bool(cache_probe.stdout.strip())
    if not cache_available:
        warnings.append(
            "macOS confstr(DARWIN_USER_CACHE_DIR) is unavailable "
            f"({probe_detail(cache_probe)}); Xcode will use the attempt-local "
            "Core Foundation cache fallback."
        )

    manager_uid = probe(("launchctl", "manageruid"))
    manager_name = probe(("launchctl", "managername"))
    current_aqua = (
        manager_uid.returncode == 0
        and manager_uid.stdout.strip() == str(resolved_uid)
        and manager_name.returncode == 0
        and manager_name.stdout.strip().lower() == "aqua"
    )
    if current_aqua:
        return HostContext((), tuple(warnings), True, cache_available)

    asuser_probe = probe(("launchctl", "asuser", str(resolved_uid), "/usr/bin/true"))
    if asuser_probe.returncode == 0:
        warnings.append(
            "Xcode will be launched through the logged-in user's launchd bootstrap "
            "because the current process is not in the Aqua manager."
        )
        return HostContext(
            ("launchctl", "asuser", str(resolved_uid)),
            tuple(warnings),
            True,
            cache_available,
        )

    warnings.append(
        "No usable Aqua/user launchd bootstrap is available "
        f"({probe_detail(asuser_probe)}). Xcode will receive one isolated attempt, "
        "but an identical host-abort retry is disabled because it cannot change "
        "the missing service context. Run this gate from a signed-in macOS Terminal "
        "session if CoreSimulator or FSEvents cannot initialize."
    )
    return HostContext((), tuple(warnings), False, cache_available)


def resolve_xcodebuild(requested: str) -> str | None:
    resolved = shutil.which(requested)
    if resolved is None:
        return None
    if requested != "xcodebuild":
        return resolved
    xcrun = shutil.which("xcrun")
    if xcrun is None:
        return resolved
    located = run_probe((xcrun, "--find", "xcodebuild"))
    candidate = located.stdout.strip()
    if located.returncode == 0 and candidate and Path(candidate).is_file():
        return candidate
    return resolved


def is_retryable_pretest_abort(result: AttemptResult) -> bool:
    """Recognize only the observed null-artifact Xcode host abort."""
    aborted = result.status in (-signal.SIGABRT, 128 + signal.SIGABRT)
    has_fsevents_failure = all(marker in result.stderr for marker in FSEVENTS_MARKERS)
    has_cache_failure = all(marker in result.stderr for marker in CACHE_MARKERS) and any(
        marker in result.stderr for marker in CACHE_IO_MARKERS
    )
    attempt_output = f"{result.stdout}\n{result.stderr}"
    has_xctest_activity = any(marker in attempt_output for marker in XCTEST_ACTIVITY_MARKERS)
    return (
        aborted
        and not result.bundle.exists()
        and has_fsevents_failure
        and has_cache_failure
        and not has_xctest_activity
    )


def execute_with_bounded_retry(
    run_attempt: Callable[[int], AttemptResult],
    infrastructure_retries: int,
) -> tuple[AttemptResult, list[AttemptResult]]:
    if not 0 <= infrastructure_retries <= MAX_INFRASTRUCTURE_RETRIES:
        raise ValueError(
            f"infrastructure_retries must be between 0 and {MAX_INFRASTRUCTURE_RETRIES}"
        )

    discarded: list[AttemptResult] = []
    for attempt_number in range(1, infrastructure_retries + 2):
        result = run_attempt(attempt_number)
        if attempt_number <= infrastructure_retries and is_retryable_pretest_abort(result):
            discarded.append(result)
            print(
                "⚠️  Xcode aborted before tests while initializing host filesystem state; "
                f"retrying once with a fresh isolated work directory (attempt {attempt_number + 1}/2).",
                file=sys.stderr,
            )
            continue
        return result, discarded
    raise AssertionError("bounded attempt loop did not return a result")


@contextmanager
def exclusive_runner_lock(lock_path: Path, timeout_seconds: float) -> Iterator[None]:
    """Serialize mutations of latest/previous and shared iOS result state."""
    if not math.isfinite(timeout_seconds) or timeout_seconds < 0:
        raise ValueError("lock timeout must be finite and non-negative")
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    lock_file = lock_path.open("a+", encoding="utf-8")
    deadline = time.monotonic() + timeout_seconds
    try:
        while True:
            try:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise RunnerBusyError(
                        f"another Magios test runner holds {lock_path}"
                    )
                time.sleep(0.1)
        yield
    finally:
        try:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
        finally:
            lock_file.close()


def atomic_write_json(path: Path, payload: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        prefix=f".{path.name}.",
        dir=path.parent,
        delete=False,
    ) as temporary:
        json.dump(payload, temporary, indent=2, sort_keys=True)
        temporary.write("\n")
        temporary_path = Path(temporary.name)
    os.replace(temporary_path, path)


def publish_run_manifests(
    *,
    args: argparse.Namespace,
    report_dir: Path,
    reports_dir: Path,
    run_id: str,
    payload: dict[str, object],
    publish_stable: bool,
) -> None:
    run_manifest = reports_dir / f"Magios-{run_id}.json"
    atomic_write_json(run_manifest, payload)
    if args.run_manifest is not None:
        atomic_write_json(args.run_manifest, payload)
    if publish_stable:
        atomic_write_json(report_dir / "latest.json", payload)


def quarantine_latest(latest: Path) -> Path | None:
    """Remove the stable current-run name without discarding the prior report."""
    if not latest.exists():
        return None
    previous = latest.with_name(f"previous{latest.suffix}")
    os.replace(latest, previous)
    return previous


def publish_report(report: Path, latest: Path, run_started_epoch: float) -> bool:
    """Atomically publish only a report produced during this invocation."""
    if not report.is_file() or report.stat().st_mtime < run_started_epoch - 1:
        return False
    latest.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="wb", prefix=f".{latest.name}.", dir=latest.parent, delete=False
    ) as temporary:
        temporary_path = Path(temporary.name)
        with report.open("rb") as source:
            shutil.copyfileobj(source, temporary)
    os.replace(temporary_path, latest)
    return True


def print_relevant_stderr(stderr: str) -> None:
    for line in stderr.splitlines():
        if not any(noise in line for noise in FILTERED_DEVICE_NOISE):
            print(line, file=sys.stderr)


def ensure_writable(directory: Path) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", prefix=".magios-write-probe-", dir=directory
    ) as probe:
        probe.write("ok")
        probe.flush()


def preserve_real_home_state(attempt_home: Path, real_home: Path) -> None:
    """Bridge only durable Xcode/Simulator state; never bridge mutable caches."""
    if not real_home.is_dir() or real_home.resolve() == attempt_home.resolve():
        return
    for relative in PRESERVED_REAL_HOME_DIRECTORIES:
        source = real_home / relative
        target = attempt_home / relative
        if source.is_dir() and not target.exists():
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(source, target_is_directory=True)
    for relative in PRESERVED_REAL_HOME_FILES:
        source = real_home / relative
        target = attempt_home / relative
        if source.is_file() and not target.exists():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)


def real_user_home() -> Path:
    """Resolve the account home independently of an inherited, redirected HOME."""
    try:
        return Path(pwd.getpwuid(os.getuid()).pw_dir)
    except (KeyError, OSError):
        return Path.home()


def attempt_environment(
    attempt_root: Path,
    *,
    real_home: Path | None = None,
) -> dict[str, str]:
    temporary = attempt_root / "tmp"
    cache = attempt_root / "cache"
    attempt_home = attempt_root / "home"
    cf_cache = attempt_home / "Library" / "Caches"
    clang_cache = cache / "clang-modules"
    swift_cache = cache / "swift-modules"
    for directory in (
        temporary,
        cache,
        attempt_home,
        cf_cache,
        clang_cache,
        swift_cache,
    ):
        ensure_writable(directory)

    source_home = real_home
    if source_home is None:
        source_home = real_user_home()
    preserve_real_home_state(attempt_home, source_home)

    environment = os.environ.copy()
    for variable in INHERITED_HOST_CONTEXT_VARIABLES:
        environment.pop(variable, None)
    for variable in tuple(environment):
        if variable.startswith("DYLD_"):
            environment.pop(variable, None)
    environment.update(
        {
            "HOME": str(attempt_home),
            "CFFIXED_USER_HOME": str(attempt_home),
            "TMPDIR": f"{temporary}{os.sep}",
            "TMP": str(temporary),
            "TEMP": str(temporary),
            "DARWIN_USER_DIR": f"{attempt_home}{os.sep}",
            "DARWIN_USER_CACHE_DIR": f"{cf_cache}{os.sep}",
            "DARWIN_USER_TEMP_DIR": f"{temporary}{os.sep}",
            "XDG_CACHE_HOME": str(cache),
            "CLANG_MODULE_CACHE_PATH": str(clang_cache),
            "SWIFT_MODULECACHE_PATH": str(swift_cache),
        }
    )
    return environment


def run_xcode_attempt(
    *,
    number: int,
    args: argparse.Namespace,
    run_id: str,
    run_work: Path,
    results_dir: Path,
    xcodebuild: str,
    launch_prefix: Sequence[str],
) -> AttemptResult:
    attempt_root = run_work / f"attempt-{number}"
    derived_data = attempt_root / "DerivedData"
    packages = attempt_root / "SourcePackages"
    for directory in (attempt_root, derived_data, packages):
        ensure_writable(directory)

    bundle = results_dir / f"Magios-{run_id}-attempt{number}.xcresult"
    stderr_path = results_dir / f"Magios-{run_id}-attempt{number}.stderr.log"
    command = [
        *launch_prefix,
        xcodebuild,
        "-project",
        args.project,
        "-scheme",
        args.scheme,
        "-configuration",
        args.configuration,
        "-destination",
        args.destination,
        "-enableCodeCoverage",
        "YES",
        "-skip-testing:MagiosUITests",
        "-derivedDataPath",
        str(derived_data),
        "-clonedSourcePackagesDirPath",
        str(packages),
        "-resultBundlePath",
        str(bundle),
        "-quiet",
        "test",
    ]
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        errors="replace",
        env=attempt_environment(attempt_root),
        stdin=subprocess.DEVNULL,
    )
    diagnostic_stop = threading.Event()
    capped_diagnostics: set[int] = set()
    diagnostic_watchdog = threading.Thread(
        target=watch_simctl_diagnostics,
        kwargs={
            "root_pid": process.pid,
            "timeout_seconds": args.diagnostic_timeout,
            "stop": diagnostic_stop,
            "capped_pids": capped_diagnostics,
        },
        name=f"magios-diagnostic-watch-{number}",
        daemon=True,
    )
    diagnostic_watchdog.start()
    try:
        stdout, stderr = process.communicate()
    finally:
        diagnostic_stop.set()
        diagnostic_watchdog.join(timeout=DIAGNOSTIC_POLL_SECONDS * 4)
    stdout = stdout or ""
    stderr = stderr or ""
    if capped_diagnostics:
        message = (
            "Magios runner capped Xcode's post-test simulator diagnostics at "
            f"{args.diagnostic_timeout:g}s; XCTest results remain in the xcresult bundle."
        )
        stderr = f"{stderr.rstrip()}\n{message}\n"
    stderr_path.write_text(stderr, encoding="utf-8")
    if stdout:
        print(stdout, end="", file=sys.stdout)
    print_relevant_stderr(stderr)
    return AttemptResult(number, process.returncode, bundle, stderr_path, stderr, stdout)


def report_command(
    *,
    args: argparse.Namespace,
    result: AttemptResult,
    report: Path,
    run_id: str,
    run_started_epoch: float,
    discarded: Sequence[AttemptResult],
    warnings: Sequence[str] = (),
) -> list[str]:
    command = [
        sys.executable,
        args.report_script,
        "--xcresult",
        str(result.bundle),
        "--output",
        str(report),
        "--xcode-status",
        str(shell_status(result.status)),
        "--run-id",
        run_id,
        "--run-start-epoch",
        str(run_started_epoch),
    ]
    for warning in warnings:
        command.extend(("--warning", warning))
    if discarded:
        command.extend(
            [
                "--warning",
                f"Retried {len(discarded)} pre-test Xcode host abort(s); each discarded attempt had no result bundle.",
            ]
        )
    if discarded and is_retryable_pretest_abort(result):
        command.extend(
            [
                "--warning",
                "Xcode's pre-test host filesystem abort persisted after the bounded retry.",
            ]
        )
    return command


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", required=True)
    parser.add_argument("--scheme", required=True)
    parser.add_argument("--configuration", default="Debug")
    parser.add_argument("--destination", required=True)
    parser.add_argument("--report-dir", required=True, type=Path)
    parser.add_argument("--work-root", default="/Volumes/build/magician/builds/ios-tests", type=Path)
    parser.add_argument(
        "--infrastructure-retries",
        default=MAX_INFRASTRUCTURE_RETRIES,
        type=int,
        choices=range(MAX_INFRASTRUCTURE_RETRIES + 1),
    )
    parser.add_argument("--xcodebuild", default="xcodebuild")
    parser.add_argument("--seed-script", type=Path)
    parser.add_argument(
        "--lock-timeout",
        default=DEFAULT_LOCK_TIMEOUT_SECONDS,
        type=float,
    )
    parser.add_argument(
        "--diagnostic-timeout",
        default=float(
            os.environ.get(
                "IOS_TEST_DIAGNOSTIC_TIMEOUT",
                DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS,
            )
        ),
        type=float,
        help="seconds allowed for Xcode's post-failure simctl diagnostics",
    )
    parser.add_argument(
        "--run-manifest",
        type=Path,
        default=os.environ.get("IOS_TEST_RUN_MANIFEST"),
    )
    parser.add_argument(
        "--correlation-id",
        default=os.environ.get("IOS_TEST_CORRELATION_ID"),
    )
    parser.add_argument(
        "--report-script",
        default=str(Path(__file__).with_name("magios_test_report.py")),
    )
    return parser.parse_args(argv)


def completion_payload(
    *,
    args: argparse.Namespace,
    run_id: str,
    run_started_epoch: float,
    state: str,
    exit_status: int,
    report: Path | None = None,
    latest: Path | None = None,
    final_result: AttemptResult | None = None,
    discarded: Sequence[AttemptResult] = (),
    warnings: Sequence[str] = (),
) -> dict[str, object]:
    attempts = [*discarded]
    if final_result is not None:
        attempts.append(final_result)
    return {
        "schema_version": 1,
        "run_id": run_id,
        "correlation_id": args.correlation_id,
        "started_epoch": run_started_epoch,
        "completed_epoch": time.time(),
        "state": state,
        "exit_status": exit_status,
        "report_path": str(report.resolve()) if report is not None else None,
        "latest_path": str(latest.resolve()) if latest is not None else None,
        "result_bundle_path": (
            str(final_result.bundle.resolve())
            if final_result is not None and final_result.bundle.exists()
            else None
        ),
        "attempts": [
            {
                "number": attempt.number,
                "xcode_status": shell_status(attempt.status),
                "stderr_path": str(attempt.stderr_path.resolve()),
                "result_bundle_path": (
                    str(attempt.bundle.resolve()) if attempt.bundle.exists() else None
                ),
            }
            for attempt in attempts
        ],
        "warnings": list(warnings),
    }


def run_locked(
    *,
    args: argparse.Namespace,
    run_id: str,
    run_started_epoch: float,
    results_dir: Path,
    reports_dir: Path,
    latest: Path,
) -> int:
    previous = quarantine_latest(latest)
    quarantine_latest(args.report_dir / "latest.json")
    if previous is not None:
        print(f"Previous iOS report retained at {previous.resolve().as_uri()}")

    xcodebuild = resolve_xcodebuild(args.xcodebuild)
    if xcodebuild is None:
        print("⏭  xcodebuild not found — skipping Magios tests (macOS + Xcode only).")
        publish_run_manifests(
            args=args,
            report_dir=args.report_dir,
            reports_dir=reports_dir,
            run_id=run_id,
            payload=completion_payload(
                args=args,
                run_id=run_id,
                run_started_epoch=run_started_epoch,
                state="skipped",
                exit_status=0,
            ),
            publish_stable=True,
        )
        return 0

    print("🧪 iOS unit tests (Magios, coverage enabled; UI smoke suite runs via test-ios-ui)...")
    host_context = inspect_host_context()
    host_warnings = list(host_context.warnings)
    for warning in host_warnings:
        print(f"⚠️  {warning}", file=sys.stderr)

    if args.seed_script is not None:
        try:
            seed_process = subprocess.run(
                ["bash", str(args.seed_script)],
                check=False,
            )
        except OSError as error:
            seed_status = 2
            host_warnings.append(f"Magios test preflight could not start: {error}")
        else:
            seed_status = shell_status(seed_process.returncode)
        if seed_status != 0:
            print(
                f"Magios test preflight failed with status {seed_status}; "
                "no current report was published.",
                file=sys.stderr,
            )
            publish_run_manifests(
                args=args,
                report_dir=args.report_dir,
                reports_dir=reports_dir,
                run_id=run_id,
                payload=completion_payload(
                    args=args,
                    run_id=run_id,
                    run_started_epoch=run_started_epoch,
                    state="preflight_failed",
                    exit_status=seed_status,
                    warnings=host_warnings,
                ),
                publish_stable=True,
            )
            return seed_status

    effective_retries = (
        args.infrastructure_retries if host_context.aqua_available else 0
    )
    run_work: Path | None = None
    try:
        ensure_writable(args.work_root)
        created_work = Path(tempfile.mkdtemp(prefix=f"{run_id}-", dir=args.work_root))
        run_work = created_work
        final_result, discarded = execute_with_bounded_retry(
            lambda number: run_xcode_attempt(
                number=number,
                args=args,
                run_id=run_id,
                run_work=created_work,
                results_dir=results_dir,
                xcodebuild=xcodebuild,
                launch_prefix=host_context.launch_prefix,
            ),
            effective_retries,
        )
    except OSError as error:
        print(f"Failed to prepare or run isolated Magios tests: {error}", file=sys.stderr)
        host_warnings.append(f"Failed to prepare or run isolated Magios tests: {error}")
        publish_run_manifests(
            args=args,
            report_dir=args.report_dir,
            reports_dir=reports_dir,
            run_id=run_id,
            payload=completion_payload(
                args=args,
                run_id=run_id,
                run_started_epoch=run_started_epoch,
                state="infrastructure_failed",
                exit_status=2,
                warnings=host_warnings,
            ),
            publish_stable=True,
        )
        return 2
    finally:
        if run_work is not None:
            shutil.rmtree(run_work, ignore_errors=True)

    report = reports_dir / f"Magios-{run_id}.html"
    try:
        report_process = subprocess.run(
            report_command(
                args=args,
                result=final_result,
                report=report,
                run_id=run_id,
                run_started_epoch=run_started_epoch,
                discarded=discarded,
                warnings=host_warnings,
            ),
            check=False,
        )
        report_process_status = shell_status(report_process.returncode)
    except OSError as error:
        report_process_status = 2
        host_warnings.append(f"Failed to start the iOS report generator: {error}")
        print(host_warnings[-1], file=sys.stderr)

    latest_published = publish_report(report, latest, run_started_epoch)
    if not latest_published:
        print(
            f"Current-run iOS report was not produced; refusing to publish stale {latest}.",
            file=sys.stderr,
        )
        report_status = report_process_status or 2
    else:
        report_status = report_process_status
        uri = latest.resolve().as_uri()
        print()
        print(f"📊 Latest iOS test report: {uri}")
        if sys.stdout.isatty():
            print(f"\033]8;;{uri}\033\\Open latest iOS test report\033]8;;\033\\")

    test_status = shell_status(final_result.status)
    exit_status = test_status if test_status != 0 else report_status
    current_report = (
        report
        if report.is_file() and report.stat().st_mtime >= run_started_epoch - 1
        else None
    )
    publish_run_manifests(
        args=args,
        report_dir=args.report_dir,
        reports_dir=reports_dir,
        run_id=run_id,
        payload=completion_payload(
            args=args,
            run_id=run_id,
            run_started_epoch=run_started_epoch,
            state="passed" if exit_status == 0 else "failed",
            exit_status=exit_status,
            report=current_report,
            latest=latest if latest_published else None,
            final_result=final_result,
            discarded=discarded,
            warnings=host_warnings,
        ),
        publish_stable=True,
    )
    return exit_status


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.run_manifest is not None:
        args.run_manifest = Path(args.run_manifest)
    if not math.isfinite(args.lock_timeout) or args.lock_timeout < 0:
        print("Magios runner lock timeout must be finite and non-negative.", file=sys.stderr)
        return 2
    if not math.isfinite(args.diagnostic_timeout) or args.diagnostic_timeout <= 0:
        print("Magios diagnostic timeout must be finite and positive.", file=sys.stderr)
        return 2

    run_started_epoch = time.time()
    run_id = datetime.now().strftime("%Y%m%d-%H%M%S") + f"-{os.getpid()}"
    results_dir = args.report_dir / "results"
    reports_dir = args.report_dir / "reports"
    latest = args.report_dir / "latest.html"
    for directory in (args.report_dir, results_dir, reports_dir):
        ensure_writable(directory)

    try:
        with exclusive_runner_lock(
            args.report_dir / ".runner.lock", args.lock_timeout
        ):
            return run_locked(
                args=args,
                run_id=run_id,
                run_started_epoch=run_started_epoch,
                results_dir=results_dir,
                reports_dir=reports_dir,
                latest=latest,
            )
    except RunnerBusyError as error:
        status = 75
        warning = (
            f"{error}; refusing a concurrent run so latest/previous artifacts remain "
            "owned by one invocation."
        )
        print(warning, file=sys.stderr)
        publish_run_manifests(
            args=args,
            report_dir=args.report_dir,
            reports_dir=reports_dir,
            run_id=run_id,
            payload=completion_payload(
                args=args,
                run_id=run_id,
                run_started_epoch=run_started_epoch,
                state="busy",
                exit_status=status,
                warnings=(warning,),
            ),
            publish_stable=False,
        )
        return status


if __name__ == "__main__":
    raise SystemExit(main())

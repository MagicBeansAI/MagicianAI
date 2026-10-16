#!/usr/bin/env python3
"""Runtime latency, memory, growth, and crash benchmark for Magician.

The correctness evaluators remain the authority for answer quality.  This lane
orchestrates those evaluators while sampling the already-running Magician
process and its bounded durable surfaces.  It also drives the two Tutor source
surfaces that historically exercised different runtime stacks.

No service is started, restarted, or killed by this script.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import ctypes
import html
import json
import math
import os
import platform
import signal
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Iterable


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_API_BASE_URL = "http://127.0.0.1:3002"
DEFAULT_DATA_ROOT = Path.home() / "MagicianNotes"
DEFAULT_LLM_PROFILE = "gpt6luna-responses-toolsany"
DEFAULT_OUTPUT_DIR = Path("coverage/evals/runtime-performance/live/latest")
DEFAULT_SAMPLE_INTERVAL_SECONDS = 0.25
MAX_REPORT_BYTES = 64 * 1024 * 1024
DEFAULT_SCENARIOS = (
    "web-researcher",
    "preplan-hitl",
    "tutor-web",
    "tutor-mobile",
    "attention-polling",
    "retrieval-contention",
)
REQUIRED_SCENARIO_METRICS = {
    "web-researcher": (
        "direct_answer_ready_ms",
        "delegated_answer_ready_ms",
        "input_tokens",
        "context_input_tokens_max",
        "context_continuation_growth_tokens_max",
        "llm_queue_wait_ms_p95",
    ),
    "preplan-hitl": (
        "case_duration_p95_ms",
        "hitl_count",
        "attention_visible_p95_ms",
        "input_tokens",
        "llm_queue_wait_ms_p95",
    ),
    "tutor-web": ("answer_ready_ms", "input_tokens"),
    "tutor-mobile": ("answer_ready_ms", "input_tokens"),
    "attention-polling": ("latency_p95_ms", "requests"),
    "retrieval-contention": (
        "concurrent_wall_p95_ms",
        "optional_background_concurrent_wall_p95_ms",
        "memory_index_write_concurrent_wall_p95_ms",
    ),
}
CRASH_MARKERS = (
    "has overflowed its stack",
    "stack overflow",
    "fatal runtime error: stack overflow",
    "sigabrt",
    "magician exited unexpectedly",
)


class EvalFailure(RuntimeError):
    pass


@dataclass
class Gate:
    name: str
    status: str
    detail: str
    scenario: str | None = None
    metric: str | None = None
    current: float | int | None = None
    baseline: float | int | None = None
    limit: float | int | None = None
    required: bool = True


@dataclass
class StorageUsage:
    logical_bytes: int = 0
    allocated_bytes: int = 0
    files: int = 0
    entries_visited: int = 0
    truncated: bool = False
    errors: list[str] = field(default_factory=list)


@dataclass
class ProcessSample:
    captured_at_ms: int
    rss_bytes: int | None
    pid_alive: bool


@dataclass
class ProcessSummary:
    pid: int | None
    sample_count: int
    available_samples: int
    start_rss_bytes: int | None
    peak_rss_bytes: int | None
    end_rss_bytes: int | None
    peak_growth_bytes: int | None
    end_growth_bytes: int | None
    process_disappeared: bool


@dataclass
class ScenarioResult:
    scenario: str
    status: str
    duration_ms: float
    command: list[str] = field(default_factory=list)
    report_path: str | None = None
    metrics: dict[str, Any] = field(default_factory=dict)
    process: dict[str, Any] = field(default_factory=dict)
    error: str | None = None


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def percentile(values: Iterable[float], quantile: float) -> float | None:
    ordered = sorted(float(value) for value in values)
    if not ordered:
        return None
    rank = max(0, math.ceil(quantile * len(ordered)) - 1)
    return ordered[min(rank, len(ordered) - 1)]


def mib(value: int | float | None) -> float | None:
    return None if value is None else float(value) / (1024 * 1024)


def safe_tail(value: str, limit: int = 4000) -> str:
    return value if len(value) <= limit else value[-limit:]


def load_json(path: Path) -> dict[str, Any]:
    try:
        size = path.stat().st_size
        if size > MAX_REPORT_BYTES:
            raise EvalFailure(
                f"JSON report {path} is {size} bytes; limit is {MAX_REPORT_BYTES}"
            )
        with path.open("rb") as handle:
            raw = handle.read(MAX_REPORT_BYTES + 1)
        if len(raw) > MAX_REPORT_BYTES:
            raise EvalFailure(f"JSON report {path} exceeded {MAX_REPORT_BYTES} bytes")
        payload = json.loads(raw)
    except EvalFailure:
        raise
    except (OSError, json.JSONDecodeError, RecursionError) as error:
        raise EvalFailure(f"cannot read JSON report {path}: {error}") from error
    if not isinstance(payload, dict):
        raise EvalFailure(f"JSON report {path} is not an object")
    return payload


def bounded_path_usage(path: Path, max_entries: int) -> StorageUsage:
    """Iteratively measure a file/tree without following symlinks."""
    usage = StorageUsage()
    pending = [path]
    while pending:
        current = pending.pop()
        if usage.entries_visited >= max_entries:
            usage.truncated = True
            break
        usage.entries_visited += 1
        try:
            info = current.lstat()
        except FileNotFoundError:
            continue
        except OSError as error:
            usage.errors.append(f"{current}: {error}")
            continue
        if current.is_symlink():
            continue
        usage.logical_bytes += int(info.st_size)
        usage.allocated_bytes += int(getattr(info, "st_blocks", 0)) * 512
        if current.is_file():
            usage.files += 1
            continue
        if not current.is_dir():
            continue
        try:
            with os.scandir(current) as entries:
                children = [Path(entry.path) for entry in entries]
        except OSError as error:
            usage.errors.append(f"{current}: {error}")
            continue
        pending.extend(children)
    return usage


def storage_targets(data_root: Path, principal: str, workspace: str) -> dict[str, Path]:
    scope = data_root / "scopes" / principal / workspace
    return {
        "attention_db": data_root / "attention_learning.db",
        "attention_db_wal": data_root / "attention_learning.db-wal",
        "chat": scope / "chat",
        "executions": scope / "executions",
        "tasks": scope / "tasks",
        "events": scope / "events.jsonl",
    }


def capture_storage(
    data_root: Path, principal: str, workspace: str, max_entries: int
) -> dict[str, dict[str, Any]]:
    return {
        name: asdict(bounded_path_usage(path, max_entries))
        for name, path in storage_targets(data_root, principal, workspace).items()
    }


def storage_delta(
    before: dict[str, dict[str, Any]], after: dict[str, dict[str, Any]]
) -> dict[str, Any]:
    targets: dict[str, dict[str, Any]] = {}
    total_logical = 0
    total_allocated = 0
    conclusive = True
    for name in sorted(set(before) | set(after)):
        left = before.get(name, {})
        right = after.get(name, {})
        logical = int(right.get("logical_bytes") or 0) - int(left.get("logical_bytes") or 0)
        allocated = int(right.get("allocated_bytes") or 0) - int(left.get("allocated_bytes") or 0)
        truncated = bool(left.get("truncated") or right.get("truncated"))
        errors = [*(left.get("errors") or []), *(right.get("errors") or [])]
        if truncated or errors:
            conclusive = False
        targets[name] = {
            "logical_bytes": logical,
            "allocated_bytes": allocated,
            "truncated": truncated,
            "errors": errors,
        }
        total_logical += logical
        total_allocated += allocated
    return {
        "logical_bytes": total_logical,
        "allocated_bytes": total_allocated,
        "conclusive": conclusive,
        "targets": targets,
    }


def resolve_listener_pid(port: int) -> int | None:
    lsof = shutil.which("lsof")
    if lsof:
        result = subprocess.run(
            [lsof, "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"],
            capture_output=True,
            text=True,
            check=False,
        )
        for line in result.stdout.splitlines():
            if line.strip().isdigit():
                return int(line.strip())
    return None


def process_rss_bytes(pid: int) -> int | None:
    proc_status = Path(f"/proc/{pid}/status")
    if proc_status.exists():
        try:
            for line in proc_status.read_text(encoding="utf-8").splitlines():
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) * 1024
        except (OSError, ValueError, IndexError):
            return None
    if platform.system() == "Darwin":
        class ProcTaskInfo(ctypes.Structure):
            _fields_ = [
                ("virtual_size", ctypes.c_uint64),
                ("resident_size", ctypes.c_uint64),
                ("total_user", ctypes.c_uint64),
                ("total_system", ctypes.c_uint64),
                ("threads_user", ctypes.c_uint64),
                ("threads_system", ctypes.c_uint64),
                ("policy", ctypes.c_int32),
                ("faults", ctypes.c_int32),
                ("pageins", ctypes.c_int32),
                ("cow_faults", ctypes.c_int32),
                ("messages_sent", ctypes.c_int32),
                ("messages_received", ctypes.c_int32),
                ("syscalls_mach", ctypes.c_int32),
                ("syscalls_unix", ctypes.c_int32),
                ("context_switches", ctypes.c_int32),
                ("thread_count", ctypes.c_int32),
                ("running_thread_count", ctypes.c_int32),
                ("priority", ctypes.c_int32),
            ]

        try:
            libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
            info = ProcTaskInfo()
            proc_pidinfo = libproc.proc_pidinfo
            proc_pidinfo.argtypes = [
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_uint64,
                ctypes.c_void_p,
                ctypes.c_int,
            ]
            proc_pidinfo.restype = ctypes.c_int
            size = ctypes.sizeof(info)
            read = proc_pidinfo(pid, 4, 0, ctypes.byref(info), size)
            if read == size:
                return int(info.resident_size)
            return None
        except (OSError, AttributeError):
            pass
    ps = shutil.which("ps")
    if not ps:
        return None
    result = subprocess.run(
        [ps, "-o", "rss=", "-p", str(pid)], capture_output=True, text=True, check=False
    )
    value = result.stdout.strip()
    return int(value) * 1024 if result.returncode == 0 and value.isdigit() else None


class ProcessSampler:
    def __init__(self, pid: int | None, interval_seconds: float) -> None:
        self.pid = pid
        self.interval_seconds = interval_seconds
        self.samples: list[ProcessSample] = []
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def capture(self) -> None:
        rss = process_rss_bytes(self.pid) if self.pid is not None else None
        self.samples.append(
            ProcessSample(
                captured_at_ms=int(time.time() * 1000),
                rss_bytes=rss,
                pid_alive=rss is not None,
            )
        )

    def start(self) -> None:
        self.capture()

        def loop() -> None:
            while not self._stop.wait(self.interval_seconds):
                self.capture()

        self._thread = threading.Thread(target=loop, name="runtime-eval-rss", daemon=True)
        self._thread.start()

    def stop(self) -> ProcessSummary:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=max(1.0, self.interval_seconds * 4))
        self.capture()
        available = [sample.rss_bytes for sample in self.samples if sample.rss_bytes is not None]
        start = next((sample.rss_bytes for sample in self.samples if sample.rss_bytes is not None), None)
        end = next(
            (sample.rss_bytes for sample in reversed(self.samples) if sample.rss_bytes is not None),
            None,
        )
        peak = max(available) if available else None
        return ProcessSummary(
            pid=self.pid,
            sample_count=len(self.samples),
            available_samples=len(available),
            start_rss_bytes=start,
            peak_rss_bytes=peak,
            end_rss_bytes=end,
            peak_growth_bytes=None if start is None or peak is None else max(0, peak - start),
            end_growth_bytes=None if start is None or end is None else end - start,
            process_disappeared=self.pid is not None and any(not sample.pid_alive for sample in self.samples),
        )


class HttpClient:
    def __init__(self, base_url: str, principal: str, workspace: str, timeout: float) -> None:
        self.base_url = base_url.rstrip("/")
        self.principal = principal
        self.workspace = workspace
        self.timeout = timeout

    def request(
        self,
        method: str,
        path: str,
        payload: dict[str, Any] | None = None,
        max_bytes: int = 16 * 1024 * 1024,
    ) -> tuple[int, bytes, float]:
        body = None if payload is None else json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=body,
            method=method,
            headers={
                "Accept": "application/json",
                "Content-Type": "application/json",
                **(
                    {"Authorization": f"Bearer {os.environ['MAGICIAN_BEARER_TOKEN'].strip()}"}
                    if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
                    else {}
                ),
            },
        )
        started = time.perf_counter()
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read(max_bytes + 1)
                if len(raw) > max_bytes:
                    raise EvalFailure(f"HTTP response exceeded {max_bytes} bytes for {path}")
                return response.status, raw, (time.perf_counter() - started) * 1000
        except urllib.error.HTTPError as error:
            raw = error.read(max_bytes)
            raise EvalFailure(
                f"HTTP {error.code} for {method} {path}: {safe_tail(raw.decode('utf-8', errors='replace'))}"
            ) from error
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            raise EvalFailure(f"HTTP unavailable for {method} {path}: {error}") from error

    def json(
        self, method: str, path: str, payload: dict[str, Any] | None = None
    ) -> tuple[dict[str, Any], float]:
        status, raw, latency = self.request(method, path, payload)
        if status < 200 or status >= 300:
            raise EvalFailure(f"unexpected HTTP {status} for {method} {path}")
        try:
            value = json.loads(raw)
        except json.JSONDecodeError as error:
            raise EvalFailure(f"invalid JSON from {method} {path}: {error}") from error
        if not isinstance(value, dict):
            raise EvalFailure(f"non-object JSON from {method} {path}")
        return value, latency


def process_metrics(summary: ProcessSummary) -> dict[str, Any]:
    value = asdict(summary)
    value.update(
        {
            "start_rss_mib": mib(summary.start_rss_bytes),
            "peak_rss_mib": mib(summary.peak_rss_bytes),
            "end_rss_mib": mib(summary.end_rss_bytes),
            "peak_growth_mib": mib(summary.peak_growth_bytes),
            "end_growth_mib": mib(summary.end_growth_bytes),
        }
    )
    return value


def run_measured(
    scenario: str,
    pid: int | None,
    sample_interval: float,
    operation: Callable[[], dict[str, Any]],
) -> ScenarioResult:
    sampler = ProcessSampler(pid, sample_interval)
    sampler.start()
    started = time.perf_counter()
    status = "pass"
    metrics: dict[str, Any] = {}
    error: str | None = None
    try:
        metrics = operation()
    except Exception as caught:  # evaluator boundary must always publish evidence
        status = "fail"
        error = str(caught)
    duration_ms = (time.perf_counter() - started) * 1000
    summary = sampler.stop()
    if summary.process_disappeared:
        status = "fail"
        error = error or "Magician listener process disappeared during scenario"
    return ScenarioResult(
        scenario=scenario,
        status=status,
        duration_ms=duration_ms,
        metrics=metrics,
        process=process_metrics(summary),
        error=error,
    )


def run_command_scenario(
    scenario: str,
    command: list[str],
    report_path: Path,
    output_path: Path,
    pid: int | None,
    sample_interval: float,
    timeout_seconds: float,
) -> ScenarioResult:
    def operation() -> dict[str, Any]:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        # Never accept a successful subprocess that forgot to publish this
        # run's evidence by accidentally loading a report from a prior run.
        report_path.unlink(missing_ok=True)
        with output_path.open("w", encoding="utf-8") as output:
            output.write(f"$ {' '.join(command)}\n\n")
            output.flush()
            process = subprocess.Popen(
                command,
                cwd=REPO_ROOT,
                stdout=output,
                stderr=subprocess.STDOUT,
                text=True,
                start_new_session=os.name == "posix",
            )
            try:
                return_code = process.wait(timeout=timeout_seconds)
            except subprocess.TimeoutExpired as error:
                signal_scenario_process(process, signal.SIGINT)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    signal_scenario_process(process, signal.SIGTERM)
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        signal_scenario_process(process, signal.SIGKILL)
                        process.wait(timeout=5)
                raise EvalFailure(f"scenario exceeded {timeout_seconds:.0f}s") from error
        if return_code != 0:
            try:
                with output_path.open("rb") as output:
                    output.seek(max(0, output_path.stat().st_size - 4000))
                    tail = output.read().decode("utf-8", errors="replace")
            except OSError:
                tail = "command output unavailable"
            raise EvalFailure(
                f"scenario command exited {return_code}: {safe_tail(tail)}"
            )
        payload = load_json(report_path)
        return extract_source_report_metrics(scenario, payload)

    result = run_measured(scenario, pid, sample_interval, operation)
    result.command = command
    result.report_path = str(report_path) if report_path.exists() else None
    return result


def signal_scenario_process(process: subprocess.Popen[str], requested: signal.Signals) -> None:
    try:
        if os.name == "posix":
            os.killpg(process.pid, requested)
        elif requested == signal.SIGKILL:
            process.kill()
        else:
            process.terminate()
    except ProcessLookupError:
        pass


def extract_source_report_metrics(scenario: str, payload: dict[str, Any]) -> dict[str, Any]:
    summary = payload.get("summary") if isinstance(payload.get("summary"), dict) else {}
    cases = payload.get("cases") if isinstance(payload.get("cases"), list) else []
    metrics: dict[str, Any] = {
        "source_status": payload.get("status")
        or ("fail" if payload.get("passed") is False else "pass"),
        "cases": len(cases),
        "input_tokens": int(summary.get("input_tokens") or 0),
        "output_tokens": int(summary.get("output_tokens") or 0),
        "reasoning_tokens": int(summary.get("reasoning_tokens") or 0),
        "llm_calls": int(summary.get("llm_calls") or 0),
        "cost_usd": float(summary.get("cost_usd") or 0.0),
    }
    llm_calls = [
        call
        for row in cases
        if isinstance(row, dict)
        for call in (row.get("llm_calls") or [])
        if isinstance(call, dict)
    ]
    for field_name in (
        "queue_wait_ms",
        "local_prep_ms",
        "provider_execution_ms",
        "latency_ms",
    ):
        samples = [
            float(call[field_name])
            for call in llm_calls
            if isinstance(call.get(field_name), (int, float))
        ]
        if samples:
            metrics[f"llm_{field_name}_p95"] = percentile(samples, 0.95)
            metrics[f"llm_{field_name}_max"] = max(samples)
    metrics.update(llm_context_metrics(llm_calls))
    if scenario == "web-researcher":
        metrics.update(
            {
                "answer_ready_p50_ms": summary.get("answer_ready_p50_ms"),
                "answer_ready_p95_ms": summary.get("answer_ready_p95_ms"),
                "longest_answer_ready_ms": summary.get("longest_answer_ready_ms"),
                "direct_answer_ready_ms": next(
                    (
                        row.get("answer_ready_ms")
                        for row in cases
                        if isinstance(row, dict) and row.get("mode") == "direct"
                    ),
                    None,
                ),
                "delegated_answer_ready_ms": next(
                    (
                        row.get("answer_ready_ms")
                        for row in cases
                        if isinstance(row, dict) and row.get("mode") == "delegated"
                    ),
                    None,
                ),
                "phase_timings_ms": {
                    str(row.get("case_id")): row.get("phase_timings_ms") or {}
                    for row in cases
                    if isinstance(row, dict)
                },
            }
        )
    elif scenario == "preplan-hitl":
        durations = [
            float(row.get("duration_ms"))
            for row in cases
            if isinstance(row, dict) and isinstance(row.get("duration_ms"), (int, float))
        ]
        hitl = [
            event
            for row in cases
            if isinstance(row, dict)
            for event in (row.get("hitl") or [])
            if isinstance(event, dict)
        ]
        attention = [
            float(event["attention_latency_ms"])
            for event in hitl
            if isinstance(event.get("attention_latency_ms"), (int, float))
        ]
        metrics.update(
            {
                "case_duration_p95_ms": percentile(durations, 0.95),
                "hitl_count": len(hitl),
                "attention_visible_p95_ms": percentile(attention, 0.95),
                "resolved_hitl": int(summary.get("resolved_hitl") or 0),
            }
        )
    elif scenario == "retrieval-contention":
        summaries = payload.get("summaries") if isinstance(payload.get("summaries"), dict) else {}
        wall = summaries.get("concurrent_wall_ms")
        if isinstance(wall, dict):
            metrics["concurrent_wall_p50_ms"] = wall.get("p50_ms")
            metrics["concurrent_wall_p95_ms"] = wall.get("p95_ms")
        background = (
            payload.get("background_contention")
            if isinstance(payload.get("background_contention"), dict)
            else {}
        )
        for lane, report in background.items():
            if not isinstance(report, dict):
                continue
            foreground = report.get("foreground_summaries")
            wall_summary = (
                foreground.get("concurrent_wall_ms")
                if isinstance(foreground, dict)
                else None
            )
            if isinstance(wall_summary, dict):
                metrics[f"{lane}_concurrent_wall_p95_ms"] = wall_summary.get("p95_ms")
            metrics[f"{lane}_background_duration_ms"] = report.get("background_duration_ms")
    return metrics


def llm_context_metrics(calls: list[dict[str, Any]]) -> dict[str, Any]:
    token_counts = [
        float(call["input_tokens"])
        for call in calls
        if isinstance(call.get("input_tokens"), (int, float))
    ]
    if not token_counts:
        return {}
    groups: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for call in calls:
        key = (
            str(call.get("execution_id") or call.get("root_execution_id") or "unbound"),
            str(call.get("operation") or "unknown"),
        )
        groups.setdefault(key, []).append(call)
    continuation_growth: list[float] = []
    for group in groups.values():
        ordered = sorted(group, key=lambda row: float(row.get("timestamp_ms") or 0))
        prior: float | None = None
        for call in ordered:
            current_value = call.get("input_tokens")
            if not isinstance(current_value, (int, float)):
                continue
            current = float(current_value)
            projection = str(call.get("prompt_projection_mode") or "").casefold()
            iteration = str(call.get("iteration_id") or "").casefold()
            is_rebootstrap = projection == "rebootstrap" or "prompt_mode=rebootstrap" in iteration
            is_bootstrap = projection == "bootstrap" or "prompt_mode=bootstrap" in iteration
            if prior is not None and not is_bootstrap and not is_rebootstrap:
                continuation_growth.append(max(0.0, current - prior))
            prior = current
    return {
        "context_input_tokens_p95": percentile(token_counts, 0.95),
        "context_input_tokens_max": max(token_counts),
        "context_continuation_growth_tokens_max": max(continuation_growth, default=0.0),
    }


def run_tutor_scenario(
    source_surface: str,
    client: HttpClient,
    profile: str,
) -> dict[str, Any]:
    suffix = uuid.uuid4().hex
    thread_id = f"runtime-eval-{source_surface}-{suffix}"
    turn_prefix = "ios-tutor" if source_surface == "ios_tutor_overlay" else "web-tutor"
    turn_id = f"{turn_prefix}-runtime-eval-{suffix}"
    query = urllib.parse.urlencode({
        "ui_thread_id": thread_id,
        "channel": "web" if source_surface == "web" else "ios",
    })
    created, create_ms = client.json("POST", f"/api/magician/v2/chat/new?{query}", {})
    session = created.get("session")
    session_id = session.get("id") if isinstance(session, dict) else None
    if not isinstance(session_id, str) or not session_id:
        raise EvalFailure(f"Tutor session creation returned no id: {created}")
    send_started = time.perf_counter()
    response: dict[str, Any]
    cleanup_error: str | None = None
    try:
        response, _ = client.json(
            "POST",
            f"/api/magician/v2/chat/sessions/{urllib.parse.quote(session_id)}/messages",
            {
                "text": "@tutor Explain why the sky appears blue in three concise milestones.",
                "chat_turn_id": turn_id,
                "source_surface": source_surface,
                "profile": profile,
            },
        )
    finally:
        # Cleanup is part of the lifecycle/storage benchmark.  A failed delete
        # is reported without masking the primary turn error.
        try:
            client.request(
                "DELETE",
                f"/api/magician/v2/chat/sessions/{urllib.parse.quote(session_id)}",
            )
        except EvalFailure as error:
            cleanup_error = str(error)
    answer_ready_ms = (time.perf_counter() - send_started) * 1000
    assistant = response.get("assistant_message")
    messages = response.get("messages") if isinstance(response.get("messages"), list) else []
    if not isinstance(assistant, dict) and not messages:
        raise EvalFailure("Tutor turn returned neither an assistant message nor display messages")
    if cleanup_error is not None:
        raise EvalFailure(f"Tutor session cleanup failed: {cleanup_error}")
    usage = response.get("usage") if isinstance(response.get("usage"), dict) else {}
    return {
        "create_session_ms": create_ms,
        "answer_ready_ms": answer_ready_ms,
        "chat_turn_id": turn_id,
        "source_surface": source_surface,
        "display_messages": len(messages),
        "llm_calls": int(usage.get("calls") or 0),
        "input_tokens": int(usage.get("inputTokens") or usage.get("input_tokens") or 0),
        "output_tokens": int(usage.get("outputTokens") or usage.get("output_tokens") or 0),
        "reasoning_tokens": int(
            usage.get("reasoningTokens") or usage.get("reasoning_tokens") or 0
        ),
        "cost_usd": float(usage.get("costUsd") or usage.get("cost_usd") or 0.0),
    }


def run_attention_polling(
    client: HttpClient,
    requests: int,
    concurrency: int,
) -> dict[str, Any]:
    paths = (
        "/api/magician/v2/channel-assist/attention-learning/canonical-projection",
        "/api/magician/v2/channel-assist/follow-ups?limit=5",
        "/api/local-resource-governor/snapshot",
    )

    def fetch(index: int) -> tuple[str, float, int]:
        path = paths[index % len(paths)]
        status, raw, latency = client.request("GET", path)
        if status < 200 or status >= 300:
            raise EvalFailure(f"attention poll returned HTTP {status} for {path}")
        return path, latency, len(raw)

    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
        samples = list(pool.map(fetch, range(requests)))
    latencies = [latency for _, latency, _ in samples]
    by_path: dict[str, list[float]] = {}
    for path, latency, _ in samples:
        by_path.setdefault(path, []).append(latency)
    return {
        "requests": len(samples),
        "concurrency": concurrency,
        "latency_p50_ms": statistics.median(latencies),
        "latency_p95_ms": percentile(latencies, 0.95),
        "max_payload_bytes": max(size for _, _, size in samples),
        "paths": {
            path: {
                "requests": len(values),
                "latency_p50_ms": statistics.median(values),
                "latency_p95_ms": percentile(values, 0.95),
            }
            for path, values in sorted(by_path.items())
        },
    }


def read_log_segment(
    path: Path,
    start_offset: int,
    max_bytes: int,
    start_identity: tuple[int, int] | None = None,
) -> tuple[str, bool]:
    try:
        metadata = path.stat()
        size = metadata.st_size
        current_identity = (int(metadata.st_dev), int(metadata.st_ino))
        offset = (
            start_offset
            if size >= start_offset
            and (start_identity is None or start_identity == current_identity)
            else 0
        )
        available = max(0, size - offset)
        truncated = available > max_bytes
        # A crash is normally terminal and therefore near the end of the new
        # segment.  Prefer the bounded newest suffix and mark it inconclusive
        # when an unusually large segment could hide earlier evidence.
        if truncated:
            offset = max(offset, size - max_bytes)
        with path.open("rb") as handle:
            handle.seek(offset)
            raw = handle.read(max_bytes)
    except OSError:
        return "", False
    return raw.decode("utf-8", errors="replace"), truncated


def crash_markers(text: str) -> list[str]:
    folded = text.casefold()
    return [marker for marker in CRASH_MARKERS if marker in folded]


def governor_snapshot(client: HttpClient) -> dict[str, Any] | None:
    try:
        value, _ = client.json("GET", "/api/local-resource-governor/snapshot")
        return value
    except EvalFailure:
        return None


def governor_metrics(before: dict[str, Any] | None, after: dict[str, Any] | None) -> dict[str, Any]:
    def resources(snapshot: dict[str, Any] | None) -> dict[str, dict[str, Any]]:
        values = snapshot.get("resources") if isinstance(snapshot, dict) else None
        return {
            str(row.get("id")): row
            for row in values or []
            if isinstance(row, dict) and row.get("id")
        }

    left = resources(before)
    right = resources(after)
    counters_before = before.get("counters", {}) if isinstance(before, dict) else {}
    counters_after = after.get("counters", {}) if isinstance(after, dict) else {}
    counter_delta = {
        key: int(counters_after.get(key) or 0) - int(counters_before.get(key) or 0)
        for key in sorted(set(counters_before) | set(counters_after))
    }
    return {
        "available": before is not None and after is not None,
        "counter_delta": counter_delta,
        "event_append_lock_wait_ms": (right.get("event_append_lock_wait") or {}).get("current"),
        "memory_overlay_lock_wait_ms": (right.get("memory_overlay_lock_wait") or {}).get("current"),
        "runtime_event_backlog": (right.get("runtime_event_backlog") or {}).get("current"),
        "runtime_event_backlog_high_water": (right.get("runtime_event_backlog") or {}).get(
            "high_water"
        ),
        "blocking_admission_in_flight": after.get("blocking_admission_in_flight")
        if isinstance(after, dict)
        else (right.get("blocking_admission") or {}).get("current"),
        "blocking_admission_waiting": after.get("blocking_admission_waiting")
        if isinstance(after, dict)
        else None,
        "blocking_admission_high_water": after.get("blocking_admission_high_water")
        if isinstance(after, dict)
        else (right.get("blocking_admission") or {}).get("high_water"),
        "blocking_admission_wait_high_water_ms": after.get(
            "blocking_admission_wait_high_water_ms"
        )
        if isinstance(after, dict)
        else None,
    }


def flatten_numeric(prefix: str, value: Any, destination: dict[str, float]) -> None:
    if isinstance(value, bool):
        return
    if isinstance(value, (int, float)):
        destination[prefix] = float(value)
        return
    if isinstance(value, dict):
        for key, child in value.items():
            flatten_numeric(f"{prefix}.{key}", child, destination)


def metric_map(results: list[ScenarioResult], storage: dict[str, Any], process: dict[str, Any]) -> dict[str, float]:
    metrics: dict[str, float] = {}
    for result in results:
        flatten_numeric(f"scenario.{result.scenario}", result.metrics, metrics)
        for key in ("peak_rss_mib", "peak_growth_mib", "end_growth_mib"):
            value = result.process.get(key)
            if isinstance(value, (int, float)):
                metrics[f"scenario.{result.scenario}.{key}"] = float(value)
    for key in ("peak_rss_mib", "peak_growth_mib", "end_growth_mib"):
        value = process.get(key)
        if isinstance(value, (int, float)):
            metrics[f"suite.{key}"] = float(value)
    metrics["suite.storage_logical_growth_mib"] = float(storage.get("logical_bytes") or 0) / (
        1024 * 1024
    )
    metrics["suite.storage_allocated_growth_mib"] = float(
        storage.get("allocated_bytes") or 0
    ) / (1024 * 1024)
    return metrics


def baseline_metrics(
    path: Path | None, *, allow_missing: bool = False
) -> dict[str, float]:
    if path is None:
        return {}
    if allow_missing and not path.exists():
        return {}
    payload = load_json(path)
    values = payload.get("metrics")
    if not isinstance(values, dict):
        raise EvalFailure(f"baseline {path} has no metrics object")
    comparable = {
        str(key): float(value)
        for key, value in values.items()
        if isinstance(value, (int, float))
        and not isinstance(value, bool)
        and comparable_metric(str(key))
    }
    if not comparable:
        raise EvalFailure(f"baseline {path} has no comparable performance metrics")
    return comparable


def capture_baseline(path: Path, metrics: dict[str, float]) -> bool:
    """Atomically create an immutable baseline without replacing an existing one."""
    path.parent.mkdir(parents=True, exist_ok=True)
    snapshot = {
        "schema_version": 1,
        "evaluator": "runtime-performance-live-baseline",
        "captured_at": utc_now(),
        "metrics": metrics,
    }
    encoded = (json.dumps(snapshot, indent=2, sort_keys=True) + "\n").encode("utf-8")
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            # A hard link is an atomic create-if-absent operation. Unlike
            # os.replace, it cannot let a concurrent run rewrite authority.
            os.link(temporary, path)
        except FileExistsError:
            return False
        return True
    except OSError as error:
        raise EvalFailure(f"cannot capture baseline {path}: {error}") from error
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def maybe_capture_missing_baseline(
    path: Path | None,
    metrics: dict[str, float],
    preliminary_status: str,
    enabled: bool,
) -> tuple[str, Gate | None]:
    """Capture only a clean first run; never promote a failed measurement."""
    if path is None or not enabled:
        return "none", None
    if path.exists():
        return (
            "appeared_concurrently",
            Gate(
                "baseline.capture_authority",
                "inconclusive",
                f"baseline {path} appeared after preflight; this run did not compare against it",
            ),
        )
    if preliminary_status != "pass":
        return (
            "not_captured",
            Gate(
                "baseline.not_promoted",
                "measure",
                f"{preliminary_status} run was not promoted to {path}",
                required=False,
            ),
        )
    try:
        created = capture_baseline(path, metrics)
    except EvalFailure as error:
        return (
            "capture_failed",
            Gate("baseline.persisted", "inconclusive", str(error)),
        )
    if not created:
        return (
            "appeared_concurrently",
            Gate(
                "baseline.capture_authority",
                "inconclusive",
                f"baseline {path} was created concurrently; this run did not compare against it",
            ),
        )
    return (
        "captured",
        Gate("baseline.persisted", "pass", f"captured immutable baseline at {path}"),
    )


def comparable_metric(metric: str) -> bool:
    return (
        "_ms" in metric
        or "rss" in metric
        or "storage_" in metric
        or metric.endswith("input_tokens")
        or ("context_" in metric and "_tokens_" in metric)
    )


def regression_limit(metric: str, baseline: float, args: argparse.Namespace) -> float:
    if metric.endswith("input_tokens") or (
        "context_" in metric and "_tokens_" in metric
    ):
        return baseline * args.max_context_ratio + args.context_slack_tokens
    if "rss" in metric:
        return baseline * args.max_rss_ratio + args.rss_slack_mib
    if "storage_" in metric:
        return max(baseline, 0.0) * args.max_storage_ratio + args.storage_slack_mib
    return baseline * args.max_latency_ratio + args.latency_slack_ms


def build_gates(
    results: list[ScenarioResult],
    suite_process: ProcessSummary,
    storage: dict[str, Any],
    governor: dict[str, Any],
    log_available: bool,
    log_truncated: bool,
    markers: list[str],
    current_metrics: dict[str, float],
    baseline: dict[str, float],
    args: argparse.Namespace,
) -> list[Gate]:
    gates: list[Gate] = []
    for result in results:
        gates.append(
            Gate(
                "scenario.completed",
                "pass" if result.status == "pass" else "fail",
                result.error or f"completed in {result.duration_ms:.1f}ms",
                scenario=result.scenario,
            )
        )
        source_status = result.metrics.get("source_status")
        if source_status is not None:
            gates.append(
                Gate(
                    "source_report.passed",
                    "pass" if source_status == "pass" else "fail",
                    f"source evaluator status={source_status}",
                    scenario=result.scenario,
                )
            )
        for metric_name in REQUIRED_SCENARIO_METRICS.get(result.scenario, ()):
            value = result.metrics.get(metric_name)
            present = isinstance(value, (int, float)) and not isinstance(value, bool)
            gates.append(
                Gate(
                    "measurement.required_metric",
                    "pass" if present else "inconclusive",
                    f"{metric_name}={value!r}",
                    scenario=result.scenario,
                    metric=f"scenario.{result.scenario}.{metric_name}",
                )
            )
        gates.append(
            Gate(
                "process.stable",
                "fail" if result.process.get("process_disappeared") else "pass",
                f"pid={result.process.get('pid')} samples={result.process.get('available_samples')}",
                scenario=result.scenario,
            )
        )
    gates.extend(
        [
            Gate(
                "process.rss_sampled",
                "pass" if suite_process.available_samples >= 2 else "inconclusive",
                f"available_samples={suite_process.available_samples}",
            ),
            Gate(
                "process.no_restart",
                "fail" if suite_process.process_disappeared else "pass",
                f"pid={suite_process.pid}",
            ),
            Gate(
                "runtime.no_crash_signature",
                "fail"
                if markers
                else ("pass" if log_available and not log_truncated else "inconclusive"),
                f"markers={markers}; truncated={log_truncated}",
                required=args.require_log,
            ),
            Gate(
                "storage.measurement_complete",
                "pass" if storage.get("conclusive") else "inconclusive",
                "bounded target scan completed" if storage.get("conclusive") else "scan truncated or unreadable",
            ),
            Gate(
                "governor.telemetry_available",
                "pass" if governor.get("available") else "inconclusive",
                "local resource governor snapshots captured",
            ),
        ]
    )
    if args.max_peak_rss_mib > 0 and suite_process.peak_rss_bytes is not None:
        current = mib(suite_process.peak_rss_bytes) or 0.0
        gates.append(
            Gate(
                "process.peak_rss_ceiling",
                "pass" if current <= args.max_peak_rss_mib else "fail",
                f"peak={current:.1f}MiB ceiling={args.max_peak_rss_mib:.1f}MiB",
                metric="suite.peak_rss_mib",
                current=current,
                limit=args.max_peak_rss_mib,
            )
        )
    if args.max_end_growth_mib >= 0 and suite_process.end_growth_bytes is not None:
        current = mib(suite_process.end_growth_bytes) or 0.0
        gates.append(
            Gate(
                "process.recovery_growth",
                "pass" if current <= args.max_end_growth_mib else "fail",
                f"end_growth={current:.1f}MiB ceiling={args.max_end_growth_mib:.1f}MiB",
                metric="suite.end_growth_mib",
                current=current,
                limit=args.max_end_growth_mib,
            )
        )
    if args.max_storage_growth_mib >= 0 and storage.get("conclusive"):
        current = float(storage.get("allocated_bytes") or 0) / (1024 * 1024)
        gates.append(
            Gate(
                "storage.allocated_growth_ceiling",
                "pass" if current <= args.max_storage_growth_mib else "fail",
                f"allocated_growth={current:.1f}MiB ceiling={args.max_storage_growth_mib:.1f}MiB",
                metric="suite.storage_allocated_growth_mib",
                current=current,
                limit=args.max_storage_growth_mib,
            )
        )
    counter_delta = governor.get("counter_delta") if isinstance(governor, dict) else None
    if isinstance(counter_delta, dict):
        for counter in (
            "event_append_lock_pressure_observations",
            "runtime_event_backlog_would_throttle_total",
        ):
            delta = int(counter_delta.get(counter) or 0)
            gates.append(
                Gate(
                    f"governor.{counter}",
                    "pass" if delta == 0 else "fail",
                    f"new_observations={delta}",
                    current=delta,
                )
            )
    for metric, prior in sorted(baseline.items()):
        current = current_metrics.get(metric)
        if current is None:
            gates.append(
                Gate(
                    "baseline.metric_present",
                    "inconclusive",
                    f"current run did not publish baseline metric {metric}",
                    metric=metric,
                )
            )
            continue
        limit = regression_limit(metric, prior, args)
        gates.append(
            Gate(
                "baseline.no_regression",
                "pass" if current <= limit else "fail",
                f"current={current:.3f} baseline={prior:.3f} limit={limit:.3f}",
                metric=metric,
                current=current,
                baseline=prior,
                limit=limit,
            )
        )
    if not baseline:
        if args.baseline and args.capture_baseline_if_missing:
            detail = (
                f"no baseline loaded; a fully passing run will initialize {args.baseline}"
            )
        else:
            detail = "no baseline supplied; this report can be passed back with --baseline"
        gates.append(
            Gate(
                "baseline.captured",
                "measure",
                detail,
                required=False,
            )
        )
    return gates


def overall_status(gates: list[Gate]) -> str:
    if any(gate.status == "fail" for gate in gates):
        return "fail"
    if any(gate.required and gate.status == "inconclusive" for gate in gates):
        return "inconclusive"
    return "pass"


def write_report(output_dir: Path, payload: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    scenarios = "".join(
        "<tr>"
        f"<td>{html.escape(row['scenario'])}</td>"
        f"<td class='{row['status']}'>{html.escape(row['status'].upper())}</td>"
        f"<td>{row['duration_ms'] / 1000:.1f}s</td>"
        f"<td>{html.escape(str(row['process'].get('peak_rss_mib') or 'n/a'))}</td>"
        f"<td>{html.escape(row.get('error') or '')}</td>"
        "</tr>"
        for row in payload["scenarios"]
    )
    gates = "".join(
        "<tr>"
        f"<td>{html.escape(gate.get('scenario') or 'suite')}</td>"
        f"<td>{html.escape(gate['name'])}</td>"
        f"<td class='{html.escape(gate['status'])}'>{html.escape(gate['status'].upper())}</td>"
        f"<td>{html.escape(gate['detail'])}</td>"
        "</tr>"
        for gate in payload["gates"]
    )
    process = payload["process"]
    storage = payload["storage_delta"]
    document = f"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Magician runtime performance live evaluation</title>
<style>body{{font:14px/1.5 system-ui;margin:32px;background:#10131a;color:#e8edf5}}
.cards{{display:flex;gap:12px;flex-wrap:wrap}}.card{{padding:12px 16px;background:#171c26;border:1px solid #2a3344;border-radius:12px}}
table{{width:100%;border-collapse:collapse;margin:16px 0 28px}}th,td{{padding:9px;border-bottom:1px solid #2a3344;text-align:left;vertical-align:top}}
.pass{{color:#63d392}}.fail{{color:#ff7b86}}.inconclusive,.measure{{color:#f5c76b}}a{{color:#85b8ff}}</style></head><body>
<h1 class="{payload['status']}">{payload['status'].upper()} · Runtime performance live evaluation</h1>
<p>{html.escape(payload['generated_at'])} · baseline: {html.escape(payload.get('baseline') or 'not configured')}
 · state: {html.escape(payload.get('baseline_state') or 'none')}</p>
<div class="cards"><div class="card"><strong>{len(payload['scenarios'])}</strong><br>scenarios</div>
<div class="card"><strong>{process.get('peak_rss_mib') or 'n/a'}</strong><br>peak RSS MiB</div>
<div class="card"><strong>{process.get('end_growth_mib') or 'n/a'}</strong><br>end growth MiB</div>
<div class="card"><strong>{storage.get('allocated_bytes', 0) / 1048576:.1f}</strong><br>allocated growth MiB</div></div>
<h2>Scenarios</h2><table><thead><tr><th>Scenario</th><th>Status</th><th>Duration</th><th>Peak RSS MiB</th><th>Error</th></tr></thead><tbody>{scenarios}</tbody></table>
<h2>Gates</h2><table><thead><tr><th>Scope</th><th>Gate</th><th>Status</th><th>Evidence</th></tr></thead><tbody>{gates}</tbody></table>
<p><a href="report.json">Raw JSON evidence and baseline metrics</a></p></body></html>"""
    (output_dir / "report.html").write_text(document, encoding="utf-8")


def scenario_commands(args: argparse.Namespace, output_dir: Path) -> dict[str, tuple[list[str], Path]]:
    web_dir = output_dir / "scenarios" / "web-researcher"
    preplan_dir = output_dir / "scenarios" / "preplan-hitl"
    retrieval_report = output_dir / "scenarios" / "retrieval-contention" / "report.json"
    web = [
        sys.executable,
        str(REPO_ROOT / "scripts/eval-web-researcher-live.py"),
        "--api-base-url",
        args.api_base_url,
        "--principal",
        args.principal,
        "--workspace",
        args.workspace,
        "--runs",
        str(args.runs),
        "--llm-profile",
        args.llm_profile,
        "--http-timeout-secs",
        str(args.http_timeout_seconds),
        "--output-dir",
        str(web_dir),
    ]
    preplan = [
        sys.executable,
        str(REPO_ROOT / "scripts/eval-preplan-flow-live.py"),
        "--api-base-url",
        args.api_base_url,
        "--principal",
        args.principal,
        "--workspace",
        args.workspace,
        "--hitl-mode",
        "fixtures",
        "--runs",
        str(args.runs),
        "--timeout-secs",
        str(args.scenario_timeout_seconds),
        "--http-timeout-secs",
        str(args.http_timeout_seconds),
        "--output-dir",
        str(preplan_dir),
    ]
    if args.config:
        web.extend(["--config", args.config])
        preplan.extend(["--config", args.config])
    retrieval = [
        "make",
        "--no-print-directory",
        "test-chat-context-retrieval-live-eval",
        f"CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS={args.retrieval_runs}",
        f"CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS={args.retrieval_warmups}",
        f"CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS={args.retrieval_background_runs}",
        f"CHAT_CONTEXT_RETRIEVAL_LIVE_OUTPUT={retrieval_report}",
    ]
    if args.config:
        retrieval.append(f"LIVE_EVAL_CONFIG={args.config}")
    return {
        "web-researcher": (web, web_dir / "report.json"),
        "preplan-hitl": (preplan, preplan_dir / "report.json"),
        "retrieval-contention": (retrieval, retrieval_report),
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--api-base-url", default=DEFAULT_API_BASE_URL)
    parser.add_argument("--principal", default="anonymous")
    parser.add_argument("--workspace", default="default")
    parser.add_argument("--config", default="")
    parser.add_argument("--llm-profile", default=DEFAULT_LLM_PROFILE)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument(
        "--capture-baseline-if-missing",
        action="store_true",
        help="atomically capture the first successful run at --baseline; never overwrite it",
    )
    parser.add_argument("--data-root", type=Path, default=DEFAULT_DATA_ROOT)
    parser.add_argument("--log-path", type=Path, default=REPO_ROOT / "magician.log")
    parser.add_argument("--require-log", action="store_true")
    parser.add_argument("--pid", type=int)
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--scenario", action="append", choices=DEFAULT_SCENARIOS)
    parser.add_argument("--scenario-timeout-seconds", type=float, default=1200)
    parser.add_argument("--http-timeout-seconds", type=float, default=600)
    parser.add_argument("--sample-interval-seconds", type=float, default=DEFAULT_SAMPLE_INTERVAL_SECONDS)
    parser.add_argument("--recovery-seconds", type=float, default=5)
    parser.add_argument("--attention-requests", type=int, default=24)
    parser.add_argument("--attention-concurrency", type=int, default=4)
    parser.add_argument("--retrieval-runs", type=int, default=5)
    parser.add_argument("--retrieval-warmups", type=int, default=1)
    parser.add_argument("--retrieval-background-runs", type=int, default=3)
    parser.add_argument("--max-storage-scan-entries", type=int, default=100_000)
    parser.add_argument("--max-log-bytes", type=int, default=8 * 1024 * 1024)
    parser.add_argument("--max-peak-rss-mib", type=float, default=0)
    parser.add_argument("--max-end-growth-mib", type=float, default=256)
    parser.add_argument("--max-storage-growth-mib", type=float, default=256)
    parser.add_argument("--max-latency-ratio", type=float, default=1.25)
    parser.add_argument("--latency-slack-ms", type=float, default=1500)
    parser.add_argument("--max-rss-ratio", type=float, default=1.20)
    parser.add_argument("--rss-slack-mib", type=float, default=64)
    parser.add_argument("--max-storage-ratio", type=float, default=1.20)
    parser.add_argument("--storage-slack-mib", type=float, default=16)
    parser.add_argument("--max-context-ratio", type=float, default=1.10)
    parser.add_argument("--context-slack-tokens", type=float, default=1000)
    args = parser.parse_args(argv)
    if args.runs < 1 or args.attention_requests < 1 or args.attention_concurrency < 1:
        parser.error("runs, attention requests, and attention concurrency must be positive")
    if args.runs > 10:
        parser.error("runs must not exceed 10")
    if args.attention_requests > 1000 or args.attention_concurrency > 32:
        parser.error("attention requests/concurrency must not exceed 1000/32")
    if args.sample_interval_seconds < 0.05 or args.scenario_timeout_seconds <= 0:
        parser.error("sample interval and scenario timeout must be positive")
    if args.recovery_seconds < 0 or args.recovery_seconds > 60:
        parser.error("recovery seconds must be between 0 and 60")
    if not 1 <= args.max_storage_scan_entries <= 1_000_000:
        parser.error("storage scan entries must be between 1 and 1000000")
    if not 1024 <= args.max_log_bytes <= 64 * 1024 * 1024:
        parser.error("max log bytes must be between 1024 and 67108864")
    if args.pid is not None and args.pid <= 0:
        parser.error("pid must be positive")
    for ratio in (
        args.max_latency_ratio,
        args.max_rss_ratio,
        args.max_storage_ratio,
        args.max_context_ratio,
    ):
        if ratio <= 0:
            parser.error("comparison ratios must be positive")
    for slack in (
        args.latency_slack_ms,
        args.rss_slack_mib,
        args.storage_slack_mib,
        args.context_slack_tokens,
    ):
        if slack < 0:
            parser.error("comparison slack values must be non-negative")
    return args


def synthetic_self_test(args: argparse.Namespace) -> tuple[list[ScenarioResult], dict[str, Any], ProcessSummary]:
    seed_results = [
        ScenarioResult(
            scenario="web-researcher",
            status="pass",
            duration_ms=250,
            metrics={
                "answer_ready_p95_ms": 250,
                "input_tokens": 1200,
                "direct_answer_ready_ms": 200,
                "delegated_answer_ready_ms": 250,
            },
            process={
                "pid": 42,
                "available_samples": 3,
                "process_disappeared": False,
                "peak_rss_mib": 128,
                "peak_growth_mib": 16,
                "end_growth_mib": 2,
            },
        ),
        ScenarioResult(
            scenario="tutor-mobile",
            status="pass",
            duration_ms=180,
            metrics={"answer_ready_ms": 180, "input_tokens": 800},
            process={
                "pid": 42,
                "available_samples": 3,
                "process_disappeared": False,
                "peak_rss_mib": 132,
                "peak_growth_mib": 20,
                "end_growth_mib": 3,
            },
        ),
    ]
    by_name = {result.scenario: result for result in seed_results}
    results = []
    for index, scenario in enumerate(DEFAULT_SCENARIOS):
        result = by_name.get(scenario) or ScenarioResult(
                scenario=scenario,
                status="pass",
                duration_ms=200 + index,
                metrics={"source_status": "pass"},
                process={
                    "pid": 42,
                    "available_samples": 3,
                    "process_disappeared": False,
                    "peak_rss_mib": 130,
                    "peak_growth_mib": 18,
                    "end_growth_mib": 2,
                },
            )
        for metric_name in REQUIRED_SCENARIO_METRICS.get(scenario, ()):
            result.metrics.setdefault(metric_name, 200 + index)
        results.append(result)
    storage = {
        "logical_bytes": 1024,
        "allocated_bytes": 4096,
        "conclusive": True,
        "targets": {},
    }
    process = ProcessSummary(42, 4, 4, 100 * 1024**2, 132 * 1024**2, 103 * 1024**2, 32 * 1024**2, 3 * 1024**2, False)
    return results, storage, process


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    args.output_dir = args.output_dir.resolve()
    if args.baseline is not None:
        args.baseline = args.baseline.resolve()
    selected = tuple(args.scenario or DEFAULT_SCENARIOS)
    client = HttpClient(
        args.api_base_url,
        args.principal,
        args.workspace,
        args.http_timeout_seconds,
    )
    try:
        baseline = baseline_metrics(
            args.baseline,
            allow_missing=args.capture_baseline_if_missing,
        )
    except EvalFailure as error:
        print(f"runtime performance baseline preflight failed: {error}", file=sys.stderr)
        return 2
    if args.self_test:
        results, storage, suite_process = synthetic_self_test(args)
        governor = {"available": True, "counter_delta": {}, "event_append_lock_wait_ms": 0}
        markers: list[str] = []
        log_available = True
        log_truncated = False
    else:
        try:
            client.request("GET", "/health")
        except EvalFailure as error:
            print(f"runtime performance preflight failed: {error}", file=sys.stderr)
            return 2
        port = urllib.parse.urlparse(args.api_base_url).port or 80
        pid = args.pid or resolve_listener_pid(port)
        if pid is None:
            print("runtime performance preflight failed: cannot resolve Magician listener PID", file=sys.stderr)
            return 2
        log_available = args.log_path.is_file()
        log_metadata = args.log_path.stat() if log_available else None
        log_offset = log_metadata.st_size if log_metadata is not None else 0
        log_identity = (
            (int(log_metadata.st_dev), int(log_metadata.st_ino))
            if log_metadata is not None
            else None
        )
        storage_before = capture_storage(
            args.data_root, args.principal, args.workspace, args.max_storage_scan_entries
        )
        governor_before = governor_snapshot(client)
        suite_sampler = ProcessSampler(pid, args.sample_interval_seconds)
        suite_sampler.start()
        results = []
        commands = scenario_commands(args, args.output_dir)
        for scenario in selected:
            print(f"▶ runtime performance scenario: {scenario}", flush=True)
            if scenario in commands:
                command, report_path = commands[scenario]
                result = run_command_scenario(
                    scenario,
                    command,
                    report_path,
                    args.output_dir / "scenarios" / scenario / "command.log",
                    pid,
                    args.sample_interval_seconds,
                    args.scenario_timeout_seconds,
                )
            elif scenario == "tutor-web":
                result = run_measured(
                    scenario,
                    pid,
                    args.sample_interval_seconds,
                    lambda: run_tutor_scenario("web", client, args.llm_profile),
                )
            elif scenario == "tutor-mobile":
                result = run_measured(
                    scenario,
                    pid,
                    args.sample_interval_seconds,
                    lambda: run_tutor_scenario("ios_tutor_overlay", client, args.llm_profile),
                )
            elif scenario == "attention-polling":
                result = run_measured(
                    scenario,
                    pid,
                    args.sample_interval_seconds,
                    lambda: run_attention_polling(
                        client, args.attention_requests, args.attention_concurrency
                    ),
                )
            else:
                raise AssertionError(f"unhandled scenario {scenario}")
            results.append(result)
            print(
                f"  {result.status.upper()} {result.duration_ms / 1000:.1f}s "
                f"peak_rss={result.process.get('peak_rss_mib')}MiB",
                flush=True,
            )
        if args.recovery_seconds > 0:
            deadline = time.monotonic() + args.recovery_seconds
            while time.monotonic() < deadline:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                time.sleep(min(args.sample_interval_seconds, remaining))
        suite_process = suite_sampler.stop()
        governor_after = governor_snapshot(client)
        governor = governor_metrics(governor_before, governor_after)
        storage_after = capture_storage(
            args.data_root, args.principal, args.workspace, args.max_storage_scan_entries
        )
        storage = storage_delta(storage_before, storage_after)
        log_text, log_truncated = read_log_segment(
            args.log_path, log_offset, args.max_log_bytes, log_identity
        )
        markers = crash_markers(log_text)
        try:
            client.request("GET", "/health")
        except EvalFailure as error:
            results.append(
                ScenarioResult(
                    scenario="postflight-health",
                    status="fail",
                    duration_ms=0,
                    error=str(error),
                )
            )
    process = process_metrics(suite_process)
    metrics = metric_map(results, storage, process)
    gates = build_gates(
        results,
        suite_process,
        storage,
        governor,
        log_available,
        log_truncated,
        markers,
        metrics,
        baseline,
        args,
    )
    baseline_state = "compared" if baseline else "none"
    if not baseline:
        baseline_state, baseline_gate = maybe_capture_missing_baseline(
            args.baseline,
            metrics,
            overall_status(gates),
            args.capture_baseline_if_missing,
        )
        if baseline_gate is not None:
            gates.append(baseline_gate)
    status = overall_status(gates)
    payload = {
        "schema_version": 1,
        "evaluator": "runtime-performance-live",
        "mode": "self-test" if args.self_test else "live",
        "generated_at": utc_now(),
        "status": status,
        "passed": status == "pass",
        "baseline": str(args.baseline) if args.baseline else None,
        "baseline_state": baseline_state,
        "scenarios_requested": list(selected),
        "metrics": metrics,
        "process": process,
        "storage_delta": storage,
        "governor": governor,
        "crash_markers": markers,
        "gates": [asdict(gate) for gate in gates],
        "scenarios": [asdict(result) for result in results],
    }
    write_report(args.output_dir, payload)
    print(f"{status.upper()} runtime performance report: {args.output_dir / 'report.html'}")
    return 0 if status == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())

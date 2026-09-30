#!/usr/bin/env python3
"""Live end-to-end evaluator for the production task pre-plan and HITL flow.

The evaluator deliberately drives the public HTTP surface instead of calling
planner internals.  That keeps profile routing, persistence, Plan-panel reads,
Attention projection, canonical HITL dispatch, replanning, event lifecycle,
and LLM observability in the same path used by Web and iOS.

Interactive runs read HITL answers from /dev/tty.  Aggregate/CI live runs use
committed fixture answers so they cannot block after the rest of the suite has
finished.  Both modes perform the same projection and lifecycle assertions.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
import getpass
from html import escape
import json
import os
from pathlib import Path
import re
import sys
import textwrap
import time
from typing import Any, Callable, Iterable
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode
from urllib.request import Request, build_opener
import pathlib

# The router's profiles and operation_mapping live in a sibling
# `llm-router.yaml`; reading the config file alone yields neither.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from magician_config_text import read_config_text  # noqa: E402



REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = REPO_ROOT / "scripts/fixtures/preplan_live/cases.json"
DEFAULT_CONFIG_CANDIDATES = (
    Path(os.environ.get("MAGICIAN_ROOT_DIR", str(Path.home() / "MagicianNotes")))
    / "magician-config.yaml",
    REPO_ROOT / "magician-config.yaml",
)
DEFAULT_BASE_URL = "http://127.0.0.1:3002"
DEFAULT_PRINCIPAL = "anonymous"
DEFAULT_WORKSPACE = "default"
MAX_RESPONSE_BYTES = 8 * 1024 * 1024
MAX_AD_HOC_PROMPT_CHARS = 32_000
POLL_INTERVAL_SECS = 0.75

PREPLAN_OPERATIONS = {
    "task_decomposition",
    "query_analysis",
    "entity_mapping",
    "atomic_composition_outline",
    "atomic_composition",
    "tool_evaluation",
    "category_matching",
    "ask_loop_clarifier",
    "answer_interpretation",
    "question_rewriting",
    "question_curation",
    "slot_extraction",
    "parameter_extraction",
    "parameter_default_inference",
    "parameter_safety_check",
    "discovery_planning",
    "discovery_extraction",
    "placeholder_resolution",
}

TERMINAL_HITL_INPUT_TYPES = {
    "text",
    "password",
    "choice",
    "multi_choice",
    "confirmation",
    "external_action",
    "file_path",
    "guidance",
    "tool_authorization",
    "sandbox_override",
    "diff_approval",
}

class EvalFailure(RuntimeError):
    pass


@dataclass
class Gate:
    name: str
    passed: bool
    detail: str
    case_id: str = "suite"
    # None means this is a deterministic/computed gate and no API request was
    # measured.  Zero is reserved for a genuinely observed zero-duration
    # measurement instead of doubling as a missing-data sentinel.
    latency_ms: float | None = None


@dataclass
class HitlObservation:
    correlation_id: str
    source: str
    prompt: str
    task_id: str
    execution_id: str | None
    plan_panel_visible: bool
    attention_visible: bool
    plan_panel_latency_ms: float
    attention_latency_ms: float
    resolved_event_visible: bool = False
    plan_panel_cleared: bool = False
    attention_cleared: bool = False


@dataclass(frozen=True)
class TerminalHitlResponse:
    """Canonical response body produced by the interactive terminal surface."""

    input_type: str
    value: dict[str, Any]
    selected_paths: tuple[str, ...] = ()


@dataclass
class CaseResult:
    case_id: str
    input_prompt: str = ""
    task_id: str = ""
    planning_execution_ids: list[str] = field(default_factory=list)
    clarification_count: int = 0
    approval_count: int = 0
    replan_count: int = 0
    final_status: str = "unknown"
    # Provider-free harness and dry-run cases do not have a runtime duration.
    duration_ms: float | None = None
    hitl: list[HitlObservation] = field(default_factory=list)
    llm_calls: list[dict[str, Any]] = field(default_factory=list)
    plan_graph: dict[str, Any] | None = None
    error: str | None = None


@dataclass(frozen=True)
class ConfigProfile:
    provider: str | None = None
    model: str | None = None
    fallback_profile: str | None = None


@dataclass(frozen=True)
class RoutingConfig:
    path: str
    operation_mapping: dict[str, str]
    profiles: dict[str, ConfigProfile]


def _strip_yaml_comment(line: str) -> str:
    # Config scalar values used here are plain identifiers; a comment begins at
    # whitespace-#.  Avoid pretending this tiny reader is a general YAML parser.
    return re.sub(r"\s+#.*$", "", line).rstrip()


def parse_routing_config(path: Path) -> RoutingConfig:
    """Read profiles and scalar/structured default operation mappings without PyYAML."""
    text = read_config_text(path)
    lines = text.splitlines()
    in_router = False
    in_profiles = False
    in_mapping = False
    current_profile: str | None = None
    current_operation: str | None = None
    structured_operations: set[str] = set()
    profile_values: dict[str, dict[str, str]] = {}
    operation_mapping: dict[str, str] = {}

    for raw in lines:
        line = _strip_yaml_comment(raw)
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip(" "))
        stripped = line.strip()
        if indent == 2 and stripped == "router:":
            in_router = True
            in_profiles = False
            in_mapping = False
            current_profile = None
            continue
        if in_router and indent <= 2 and stripped != "router:":
            in_router = False
            in_profiles = False
            in_mapping = False
            current_profile = None
        if not in_router:
            continue
        if indent == 4 and stripped == "profiles:":
            in_profiles = True
            in_mapping = False
            current_profile = None
            continue
        if indent == 4 and stripped == "operation_mapping:":
            in_mapping = True
            in_profiles = False
            current_profile = None
            continue
        if indent <= 4:
            if stripped not in {"profiles:", "operation_mapping:"}:
                in_profiles = False
                in_mapping = False
                current_profile = None
            continue

        if in_profiles:
            if indent == 6 and stripped.endswith(":"):
                current_profile = stripped[:-1].strip()
                profile_values.setdefault(current_profile, {})
                continue
            if current_profile and ":" in stripped:
                key, value = stripped.split(":", 1)
                key, value = key.strip(), value.strip().strip("'\"")
                if key in {"provider", "model", "fallback_profile"} and value:
                    # The first provider/model belongs to the profile root.
                    profile_values[current_profile].setdefault(key, value)
        elif in_mapping:
            if indent == 6 and ":" in stripped:
                key, value = stripped.split(":", 1)
                key = key.strip().strip("'\"")
                value = value.strip().strip("'\"")
                current_operation = None
                if key in PREPLAN_OPERATIONS:
                    if value:
                        operation_mapping[key] = value
                    else:
                        current_operation = key
                        structured_operations.add(key)
            elif current_operation and indent == 8 and ":" in stripped:
                key, value = stripped.split(":", 1)
                value = value.strip().strip("'\"")
                # Metadata and conditional arms are not the default pre-plan
                # binding, and must not broaden its allowed fallback chain.
                if key.strip() == "default" and value:
                    operation_mapping[current_operation] = value

    missing_defaults = sorted(structured_operations - operation_mapping.keys())
    if missing_defaults:
        raise EvalFailure(f"pre-plan operation selectors missing default profiles: {missing_defaults}")
    if not operation_mapping:
        raise EvalFailure(f"no pre-plan operation mappings found in {path}")
    profiles = {
        name: ConfigProfile(
            provider=values.get("provider"),
            model=values.get("model"),
            fallback_profile=values.get("fallback_profile"),
        )
        for name, values in profile_values.items()
    }
    missing = sorted(set(operation_mapping.values()) - set(profiles))
    if missing:
        raise EvalFailure(f"mapped profiles missing from llm.router.profiles: {missing}")
    return RoutingConfig(str(path), operation_mapping, profiles)


def allowed_profile_chain(config: RoutingConfig, operation: str) -> list[str]:
    profile = config.operation_mapping.get(operation)
    allowed: list[str] = []
    seen: set[str] = set()
    while profile and profile not in seen:
        seen.add(profile)
        allowed.append(profile)
        profile = config.profiles.get(profile, ConfigProfile()).fallback_profile
    return allowed


def resolve_config_path(explicit: str | None) -> Path:
    candidates: list[Path] = []
    if explicit:
        candidates.append(Path(explicit).expanduser())
    if os.environ.get("MAGICIAN_CONFIG_PATH"):
        candidates.append(Path(os.environ["MAGICIAN_CONFIG_PATH"]).expanduser())
    candidates.extend(DEFAULT_CONFIG_CANDIDATES)
    for path in candidates:
        if path.is_file():
            return path.resolve()
    raise EvalFailure("no magician-config.yaml found for live routing verification")


class Client:
    def __init__(self, base_url: str, principal: str, workspace: str, timeout: float):
        self.base_url = base_url.rstrip("/")
        self.principal = principal
        self.workspace = workspace
        self.timeout = timeout
        self.opener = build_opener()

    def request(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        *,
        accept: str = "application/json",
    ) -> tuple[int, Any, float]:
        params = {k: v for k, v in (query or {}).items() if v is not None}
        suffix = f"{'&' if '?' in path else '?'}{urlencode(params)}" if params else ""
        url = f"{self.base_url}{path}{suffix}"
        data = json.dumps(body).encode("utf-8") if body is not None else None
        request = Request(
            url,
            data=data,
            method=method,
            headers={
                "Accept": accept,
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
            response = self.opener.open(request, timeout=self.timeout)
            status = response.status
            raw = response.read(MAX_RESPONSE_BYTES + 1)
        except HTTPError as error:
            status = error.code
            raw = error.read(MAX_RESPONSE_BYTES + 1)
        except (URLError, TimeoutError, OSError) as error:
            raise EvalFailure(
                f"runtime API unavailable during {method} {path} "
                f"at {self.base_url}: {error}"
            ) from error
        latency_ms = (time.perf_counter() - started) * 1000
        if len(raw) > MAX_RESPONSE_BYTES:
            raise EvalFailure(f"API response exceeded {MAX_RESPONSE_BYTES} bytes: {path}")
        if accept == "application/x-ndjson":
            rows = []
            for line in raw.decode("utf-8", errors="replace").splitlines():
                if not line.strip():
                    continue
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError as error:
                    raise EvalFailure(f"invalid NDJSON row from {path}: {line[:160]}") from error
            return status, rows, latency_ms
        try:
            payload = json.loads(raw) if raw else None
        except json.JSONDecodeError as error:
            raise EvalFailure(f"invalid JSON from {path} (HTTP {status})") from error
        return status, payload, latency_ms

    def get_json(self, path: str, query: dict[str, Any] | None = None) -> tuple[Any, float]:
        status, payload, latency = self.request("GET", path, query=query)
        if status != 200:
            raise EvalFailure(f"GET {path} failed with HTTP {status}: {payload}")
        return payload, latency

    def post_json(
        self,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        expected: Iterable[int] = (200,),
    ) -> tuple[Any, float, int]:
        status, payload, latency = self.request("POST", path, body, query)
        if status not in set(expected):
            raise EvalFailure(f"POST {path} failed with HTTP {status}: {payload}")
        return payload, latency, status


def cleanup_task(client: Client, case_id: str, task_id: str) -> list[Gate]:
    """Remove a disposable eval task, cancelling an active plan if required."""
    gates: list[Gate] = []
    status, payload, latency = client.request(
        "DELETE",
        f"/api/magician/v3/tasks/{quote(task_id)}",
        query={"remove_files": "true"},
    )
    # Active planning tasks correctly reject archival. A failed live gate can
    # leave one in `planning`/`eliciting`, so terminate it through the public
    # lifecycle API and retry the same destructive cleanup. This also resolves
    # any unanswered planning HITL before its files are removed.
    if (
        status == 400
        and isinstance(payload, dict)
        and str(payload.get("error") or "").startswith("task_active:")
    ):
        cancel_status, cancel_payload, cancel_latency = client.request(
            "PUT",
            f"/api/magician/v3/tasks/{quote(task_id)}/status",
            {"status": "cancelled"},
        )
        gates.append(
            Gate(
                "cleanup.active_task_cancelled",
                cancel_status == 200,
                f"HTTP {cancel_status} payload={cancel_payload}",
                case_id,
                cancel_latency,
            )
        )
        if cancel_status == 200:
            status, payload, retry_latency = client.request(
                "DELETE",
                f"/api/magician/v3/tasks/{quote(task_id)}",
                query={"remove_files": "true"},
            )
            latency += retry_latency
    gates.append(
        Gate(
            "cleanup.task_removed",
            status == 200
            and isinstance(payload, dict)
            and payload.get("files_removed") is True,
            f"HTTP {status} payload={payload}",
            case_id,
            latency,
        )
    )
    return gates


def _nested_string(value: Any, keys: Iterable[str]) -> str | None:
    if isinstance(value, dict):
        for key in keys:
            candidate = value.get(key)
            if isinstance(candidate, str) and candidate.strip():
                return candidate.strip()
        for candidate in value.values():
            found = _nested_string(candidate, keys)
            if found:
                return found
    elif isinstance(value, list):
        for candidate in value:
            found = _nested_string(candidate, keys)
            if found:
                return found
    return None


def task_id_from_response(payload: Any) -> str:
    task = payload.get("task") if isinstance(payload, dict) else None
    task_id = _nested_string(task, ("task_id", "id"))
    if not task_id:
        raise EvalFailure(f"task create response has no task id: {payload}")
    return task_id


def plan_record(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict) or not isinstance(payload.get("plan"), dict):
        raise EvalFailure(f"plan endpoint returned an invalid envelope: {payload}")
    return payload["plan"]


def question_id(question: dict[str, Any]) -> str:
    value = question.get("id") or question.get("question_id")
    if not isinstance(value, str) or not value.strip():
        raise EvalFailure(f"pending question has no id: {question}")
    return value.strip()


def question_prompt(question: dict[str, Any]) -> str:
    value = question.get("question_text") or question.get("question")
    return str(value).strip() if value is not None else "Planning clarification"


def attention_items(payload: Any) -> list[dict[str, Any]]:
    if not isinstance(payload, dict):
        return []
    items: list[dict[str, Any]] = []
    for lane in ("requests", "approvals", "escalations", "failed", "running"):
        rows = payload.get(lane)
        if isinstance(rows, list):
            items.extend(row for row in rows if isinstance(row, dict))
    return items


def attention_item_matches(item: dict[str, Any], correlation_id: str, task_id: str) -> bool:
    metadata = item.get("metadata") if isinstance(item.get("metadata"), dict) else {}
    aliases = {
        str(item.get("id") or ""),
        *(str(metadata.get(key) or "") for key in (
            "correlation_id", "pause_state_id", "approval_id", "request_id"
        )),
    }
    item_task = str(item.get("task_id") or metadata.get("task_id") or "")
    return correlation_id in aliases and (not item_task or item_task == task_id)


def find_attention(payload: Any, correlation_id: str, task_id: str) -> dict[str, Any] | None:
    return next(
        (
            item
            for item in attention_items(payload)
            if attention_item_matches(item, correlation_id, task_id)
        ),
        None,
    )


def attention_identity(item: dict[str, Any]) -> tuple[str | None, str | None, str]:
    """Return canonical correlation, execution, and display prompt from a feed row."""
    metadata = item.get("metadata") if isinstance(item.get("metadata"), dict) else {}
    hitl_request = (
        metadata.get("hitl_request")
        if isinstance(metadata.get("hitl_request"), dict)
        else {}
    )
    correlation = (
        _nested_string(hitl_request, ("correlation_id", "pause_state_id", "id"))
        or _nested_string(
            metadata,
            ("correlation_id", "pause_state_id", "approval_id", "request_id"),
        )
    )
    execution = (
        str(metadata.get("execution_id") or "").strip()
        or _nested_string(hitl_request, ("execution_id",))
    )
    prompt = (
        _nested_string(hitl_request, ("prompt",))
        or _nested_string(item, ("summary", "title"))
        or ""
    )
    return correlation, execution, prompt


def approval_prompts_are_equivalent(plan_prompt: str, attention_prompt: str) -> bool:
    """Plan Inspector and Attention may add a title, but must retain intent."""
    required = {"plan", "ready", "review"}
    plan_words = set(re.findall(r"[a-z]+", plan_prompt.lower()))
    attention_words = set(re.findall(r"[a-z]+", attention_prompt.lower()))
    return required.issubset(plan_words) and required.issubset(attention_words)


def event_type(event: Any) -> str:
    if not isinstance(event, dict):
        return ""
    raw = str(event.get("event_type") or event.get("type") or "")
    # Runtime envelopes can use either taxonomy spelling (`hitl.resolved`) or
    # the Rust enum spelling (`HitlResolved`). Normalize both to dotted words.
    dotted = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", ".", raw)
    return dotted.lower().replace("_", ".").replace("-", ".")


def event_matches(event: Any, correlation_id: str, kind: str) -> bool:
    normalized = event_type(event).replace("-", ".")
    kind_normalized = kind.lower().replace("_", ".")
    return kind_normalized in normalized and _nested_string(
        event, ("correlation_id", "pause_state_id", "approval_id", "request_id", "question_id")
    ) == correlation_id


def wait_until(
    description: str,
    timeout_secs: float,
    probe: Callable[[], tuple[bool, Any, float]],
) -> tuple[Any, float]:
    deadline = time.monotonic() + timeout_secs
    last: Any = None
    total_latency = 0.0
    while time.monotonic() < deadline:
        ok, last, latency = probe()
        total_latency += latency
        if ok:
            return last, total_latency
        time.sleep(POLL_INTERVAL_SECS)
    raise EvalFailure(f"timed out after {timeout_secs:.0f}s waiting for {description}; last={last}")


def poll_plan(client: Client, task_id: str) -> tuple[dict[str, Any], float]:
    payload, latency = client.get_json(f"/api/magician/v3/tasks/{quote(task_id)}/plan")
    return payload, latency


def plan_panel_questions(client: Client, task_id: str) -> tuple[list[dict[str, Any]], float]:
    """Read questions from the exact `/plan` payload used by Plan Inspector."""
    payload, latency = poll_plan(client, task_id)
    plan = plan_record(payload)
    pending = plan.get("pending_questions")
    # The public V3 response uses `skip_serializing_if = Vec::is_empty`; an
    # omitted field is therefore the canonical wire representation of an empty
    # list once a clarification clears. Preserve strict validation for a field
    # that is present with the wrong type.
    if pending is None:
        return [], latency
    if not isinstance(pending, list):
        raise EvalFailure(f"plan endpoint returned invalid pending_questions: {payload}")
    return [row for row in pending if isinstance(row, dict)], latency


def attention_payload(client: Client) -> tuple[dict[str, Any], float]:
    payload, latency = client.get_json("/api/magician/v2/feed/attention", {"limit": 200})
    if not isinstance(payload, dict):
        raise EvalFailure("Attention endpoint returned a non-object")
    return payload, latency


def execution_events(
    client: Client,
    task_id: str,
    execution_id: str | None,
    since_ms: int,
) -> tuple[list[dict[str, Any]], float]:
    query: dict[str, Any] = {
        "task_id": task_id,
        "since": since_ms,
        "limit": 2000,
        "category": "hitl",
        "backfill_only": "true",
    }
    if execution_id:
        query["execution_id"] = execution_id
    status, rows, latency = client.request(
        "GET", "/api/magician/v3/events", query=query, accept="application/x-ndjson"
    )
    if status != 200 or not isinstance(rows, list):
        raise EvalFailure(f"event backfill failed with HTTP {status}: {rows}")
    return [row for row in rows if isinstance(row, dict)], latency


def observe_pending_hitl(
    client: Client,
    case_id: str,
    task_id: str,
    execution_id: str | None,
    correlation_id: str,
    source: str,
    prompt: str,
    timeout_secs: float,
) -> tuple[HitlObservation, list[Gate]]:
    plan_latency = 0.0
    attention_latency = 0.0

    if source == "clarification":
        def plan_probe() -> tuple[bool, Any, float]:
            questions, latency = plan_panel_questions(client, task_id)
            match = next((q for q in questions if question_id(q) == correlation_id), None)
            return match is not None, match or questions, latency
    else:
        def plan_probe() -> tuple[bool, Any, float]:
            payload, latency = poll_plan(client, task_id)
            target = payload.get("hitl_request") if isinstance(payload, dict) else None
            match = isinstance(target, dict) and (
                str(target.get("id") or "") == correlation_id
                or _nested_string(target, ("correlation_id",)) == correlation_id
            )
            return match, target or payload, latency

    plan_item, plan_latency = wait_until(
        f"{source} {correlation_id} in Plan panel projection", timeout_secs, plan_probe
    )

    def attention_probe() -> tuple[bool, Any, float]:
        payload, latency = attention_payload(client)
        match = find_attention(payload, correlation_id, task_id)
        return match is not None, match or {"lanes": {k: len(payload.get(k, [])) for k in ("requests", "approvals", "escalations")}}, latency

    item, attention_latency = wait_until(
        f"{source} {correlation_id} in Attention", timeout_secs, attention_probe
    )
    metadata = item.get("metadata") if isinstance(item, dict) and isinstance(item.get("metadata"), dict) else {}
    attention_correlation, attention_execution, attention_prompt = attention_identity(item)
    plan_prompt = question_prompt(plan_item) if source == "clarification" else (
        _nested_string(plan_item, ("prompt",)) or ""
    )
    if source == "clarification":
        prompt_matches = (
            plan_prompt.strip() == prompt.strip()
            and attention_prompt.strip() == prompt.strip()
        )
    else:
        prompt_matches = approval_prompts_are_equivalent(plan_prompt, attention_prompt)
    gates = [
        Gate(
            f"{source}.plan_panel_visible",
            True,
            f"correlation={correlation_id}",
            case_id,
            plan_latency,
        ),
        Gate(
            f"{source}.attention_visible",
            bool(item),
            f"correlation={correlation_id} item={item.get('id') if isinstance(item, dict) else None}",
            case_id,
            attention_latency,
        ),
        Gate(
            f"{source}.attention_identity",
            isinstance(item, dict)
            and str(item.get("task_id") or "") == task_id
            and attention_correlation == correlation_id
            and str(metadata.get("source") or "") == source
            and (not execution_id or attention_execution == execution_id),
            f"task={item.get('task_id') if isinstance(item, dict) else None} correlation={attention_correlation} execution={attention_execution} source={metadata.get('source')}",
            case_id,
        ),
        Gate(
            f"{source}.prompt_visible",
            prompt_matches,
            f"plan_prompt={plan_prompt!r} attention_prompt={attention_prompt!r}",
            case_id,
        ),
    ]
    return HitlObservation(
        correlation_id=correlation_id,
        source=source,
        prompt=prompt,
        task_id=task_id,
        execution_id=execution_id,
        plan_panel_visible=True,
        attention_visible=bool(item),
        plan_panel_latency_ms=plan_latency,
        attention_latency_ms=attention_latency,
    ), gates


def observe_resolved_hitl(
    client: Client,
    case_id: str,
    observation: HitlObservation,
    since_ms: int,
    timeout_secs: float,
) -> list[Gate]:
    correlation_id = observation.correlation_id
    task_id = observation.task_id

    if observation.source == "clarification":
        def plan_clear_probe() -> tuple[bool, Any, float]:
            questions, latency = plan_panel_questions(client, task_id)
            present = any(question_id(q) == correlation_id for q in questions)
            return not present, questions, latency
    else:
        def plan_clear_probe() -> tuple[bool, Any, float]:
            payload, latency = poll_plan(client, task_id)
            target = payload.get("hitl_request") if isinstance(payload, dict) else None
            present = isinstance(target, dict) and (
                str(target.get("id") or "") == correlation_id
                or _nested_string(target, ("correlation_id",)) == correlation_id
            )
            return not present, payload, latency

    _, plan_latency = wait_until(
        f"resolved {correlation_id} to clear from Plan panel", timeout_secs, plan_clear_probe
    )

    def attention_clear_probe() -> tuple[bool, Any, float]:
        payload, latency = attention_payload(client)
        match = find_attention(payload, correlation_id, task_id)
        return match is None, match or {"cleared": True}, latency

    _, attention_latency = wait_until(
        f"resolved {correlation_id} to clear from Attention", timeout_secs, attention_clear_probe
    )

    def event_probe() -> tuple[bool, Any, float]:
        rows, latency = execution_events(
            client, task_id, observation.execution_id, since_ms
        )
        match = next((event for event in rows if event_matches(event, correlation_id, "hitl.resolved")), None)
        return match is not None, match or {"event_types": sorted({event_type(e) for e in rows})}, latency

    _, event_latency = wait_until(
        f"hitl.resolved event for {correlation_id}", timeout_secs, event_probe
    )
    observation.plan_panel_cleared = True
    observation.attention_cleared = True
    observation.resolved_event_visible = True
    return [
        Gate(
            f"{observation.source}.plan_panel_cleared",
            True,
            f"correlation={correlation_id}",
            case_id,
            plan_latency,
        ),
        Gate(
            f"{observation.source}.attention_cleared",
            True,
            f"correlation={correlation_id}",
            case_id,
            attention_latency,
        ),
        Gate(
            f"{observation.source}.resolved_event",
            True,
            f"correlation={correlation_id}",
            case_id,
            event_latency,
        ),
    ]


def fixture_answer(case: dict[str, Any], question: dict[str, Any]) -> str:
    searchable = " ".join(
        str(value)
        for value in (
            question.get("question_text"),
            question.get("source_slot_id"),
            question.get("related_slots"),
        )
        if value is not None
    ).lower()
    for rule in case.get("answer_rules", []):
        if not isinstance(rule, dict):
            continue
        terms = [str(term).lower() for term in rule.get("contains_any", [])]
        if terms and any(term in searchable for term in terms):
            return str(rule.get("answer") or "").strip()
    answer = str(case.get("default_answer") or "").strip()
    if not answer:
        raise EvalFailure(f"fixture case {case.get('id')} has no answer for: {searchable}")
    return answer


def terminal_line(prompt: str) -> str:
    try:
        with open("/dev/tty", "r", encoding="utf-8") as reader, open(
            "/dev/tty", "w", encoding="utf-8"
        ) as writer:
            writer.write(prompt)
            writer.flush()
            value = reader.readline()
            if value == "":
                raise EvalFailure("terminal HITL input stream closed before an answer was supplied")
            return value.rstrip("\r\n")
    except OSError as error:
        raise EvalFailure(
            "terminal HITL mode requires an attached TTY; use --hitl-mode fixtures for automation"
        ) from error


def terminal_secret(prompt: str) -> str:
    try:
        return getpass.getpass(prompt)
    except (EOFError, OSError) as error:
        raise EvalFailure(
            "terminal password HITL requires an attached TTY; use --hitl-mode fixtures for automation"
        ) from error


def _schema_record(request: dict[str, Any]) -> dict[str, Any]:
    schema = request.get("input_schema") or request.get("schema")
    return schema if isinstance(schema, dict) else {}


def _terminal_options(request: dict[str, Any], schema: dict[str, Any]) -> list[dict[str, Any]]:
    raw = schema.get("options")
    if not isinstance(raw, list):
        raw = request.get("options")
    if not isinstance(raw, list):
        return []
    options: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, row in enumerate(raw, start=1):
        if not isinstance(row, dict):
            raise EvalFailure(f"terminal HITL option {index} is not an object")
        option_id = str(row.get("id") or row.get("value") or "").strip()
        label = str(row.get("label") or option_id).strip()
        if not option_id or not label:
            raise EvalFailure(f"terminal HITL option {index} has no id/value or label")
        if option_id in seen:
            raise EvalFailure(f"terminal HITL options repeat id {option_id!r}")
        seen.add(option_id)
        options.append(
            {
                "id": option_id,
                "label": label,
                "description": str(row.get("description") or "").strip(),
                "requires_input": row.get("requires_input") is True,
            }
        )
    return options


def _abort_response(raw: str, input_type: str) -> TerminalHitlResponse | None:
    value = raw.strip()
    if value.lower() == ":abort":
        return TerminalHitlResponse(input_type, {"type": "aborted"})
    if value.lower().startswith(":abort "):
        return TerminalHitlResponse(
            input_type,
            {"type": "aborted", "reason": value[len(":abort "):].strip()},
        )
    return None


def _emit_options(options: list[dict[str, Any]], emit: Callable[[str], None]) -> None:
    emit("Options:")
    for index, option in enumerate(options, start=1):
        suffix = f" — {option['description']}" if option["description"] else ""
        input_note = " (additional input required)" if option["requires_input"] else ""
        emit(f"  [{index}] {option['label']} [{option['id']}]{input_note}{suffix}")


def _option_for_token(token: str, options: list[dict[str, Any]]) -> dict[str, Any] | None:
    token = token.strip()
    if token.isdigit():
        index = int(token)
        if 1 <= index <= len(options):
            return options[index - 1]
    folded = token.casefold()
    matches = [
        option
        for option in options
        if folded in {option["id"].casefold(), option["label"].casefold()}
    ]
    return matches[0] if len(matches) == 1 else None


def _read_multiline(
    read_line: Callable[[str], str],
    emit: Callable[[str], None],
    input_type: str,
) -> str | TerminalHitlResponse:
    emit("Enter one or more lines. Type .done on its own line to submit.")
    lines: list[str] = []
    while True:
        line = read_line("> ")
        aborted = _abort_response(line, input_type)
        if aborted is not None:
            return aborted
        if line.strip() == ".done":
            return "\n".join(lines)
        lines.append(line)


def terminal_hitl_response(
    request: dict[str, Any],
    *,
    read_line: Callable[[str], str] | None = None,
    read_secret: Callable[[str], str] | None = None,
    emit: Callable[[str], None] = print,
) -> TerminalHitlResponse:
    """Render and collect every canonical HITL input type over a TTY.

    The returned value is already shaped like `RespondHitlRequest`: callers do
    not reinterpret labels or comma-separated strings after this boundary.
    """

    read_line = read_line or terminal_line
    read_secret = read_secret or terminal_secret
    input_type = str(request.get("input_type") or "").strip()
    if input_type not in TERMINAL_HITL_INPUT_TYPES:
        raise EvalFailure(f"terminal HITL received unsupported input_type: {input_type!r}")
    schema = _schema_record(request)
    schema_type = str(schema.get("type") or "").strip()
    if schema_type and schema_type != input_type:
        raise EvalFailure(
            f"terminal HITL input_type {input_type!r} conflicts with schema type {schema_type!r}"
        )
    source = str(request.get("source") or "").strip()
    if source in {"approval", "plan_approval"} and input_type != "confirmation":
        raise EvalFailure(f"terminal {source} HITL must use confirmation input")
    if source and (source == "diff_approval") != (input_type == "diff_approval"):
        raise EvalFailure("terminal diff_approval source and input type must match")
    if input_type in {"tool_authorization", "sandbox_override"} and source and source not in {
        "agentic",
        "primitive",
        "inner_loop",
        "escalation",
    }:
        raise EvalFailure(f"terminal {input_type} HITL cannot be resolved for source {source!r}")
    prompt = str(request.get("prompt") or request.get("question_text") or "Input required").strip()
    hint = str(request.get("hint") or "").strip()

    emit(f"\nHITL asks: {prompt}")
    if hint:
        emit(f"Context: {hint}")
    emit("Type :abort or :abort <reason> at any prompt to cancel this request.")

    if input_type == "password":
        placeholder = str(schema.get("placeholder") or "Sensitive value").strip()
        while True:
            answer = read_secret(f"{placeholder}: ")
            aborted = _abort_response(answer, input_type)
            if aborted is not None:
                return aborted
            if answer:
                return TerminalHitlResponse(input_type, {"type": "password", "value": answer})
            emit("A password value is required.")

    if input_type in {"text", "guidance"}:
        if input_type == "guidance":
            context = str(schema.get("context") or "").strip()
            if context:
                emit("What has been tried: " + context)
            suggestions = schema.get("suggestions")
            if isinstance(suggestions, list) and suggestions:
                emit("Suggestions:")
                for suggestion in suggestions:
                    if str(suggestion).strip():
                        emit("  - " + str(suggestion).strip())
        multiline = input_type == "guidance" or schema.get("multiline") is True
        while True:
            if multiline:
                answer = _read_multiline(read_line, emit, input_type)
                if isinstance(answer, TerminalHitlResponse):
                    return answer
            else:
                answer = read_line(str(schema.get("placeholder") or "Your answer") + ": ")
                aborted = _abort_response(answer, input_type)
                if aborted is not None:
                    return aborted
            max_length = schema.get("max_length")
            if isinstance(max_length, int) and max_length > 0 and len(answer) > max_length:
                emit(f"Response is {len(answer)} characters; maximum is {max_length}. Try again.")
                continue
            if input_type == "guidance":
                return TerminalHitlResponse(input_type, {"type": "guidance", "advice": answer})
            return TerminalHitlResponse(input_type, {"type": "text", "value": answer})

    if input_type in {"choice", "multi_choice", "tool_authorization", "sandbox_override"}:
        options = _terminal_options(request, schema)
        if not options and input_type == "tool_authorization":
            options = [
                {"id": "allow_once", "label": "Allow once", "description": "", "requires_input": False},
                {"id": "allow_always", "label": "Allow for this run", "description": "", "requires_input": False},
                {"id": "deny", "label": "Deny", "description": "", "requires_input": False},
            ]
        if not options and input_type == "sandbox_override":
            options = [
                {"id": "allow_once", "label": "Allow once", "description": "", "requires_input": False},
                {"id": "deny", "label": "Deny", "description": "", "requires_input": False},
            ]
        if not options:
            raise EvalFailure(f"terminal {input_type} HITL has no valid options")
        if input_type == "tool_authorization":
            emit(f"Tool: {schema.get('tool_name') or '(unknown)'}")
            emit(f"Parameters: {schema.get('params_summary') or '(not supplied)'}")
        elif input_type == "sandbox_override":
            emit(f"Command: {schema.get('command') or '(unknown)'}")
            emit(f"Policy violation: {schema.get('violation') or '(not supplied)'}")
            roots = schema.get("allowed_roots")
            if isinstance(roots, list) and roots:
                emit("Allowed roots: " + ", ".join(str(root) for root in roots))
        _emit_options(options, emit)

        if input_type == "multi_choice":
            minimum = schema.get("min_selections")
            maximum = schema.get("max_selections")
            minimum = max(1, minimum if isinstance(minimum, int) else 0)
            maximum = maximum if isinstance(maximum, int) and maximum > 0 else 0
            if maximum and minimum > maximum:
                raise EvalFailure(
                    f"terminal multi_choice bounds are invalid: min={minimum}, max={maximum}"
                )
            constraint = f"Select at least {minimum}"
            if maximum:
                constraint += f" and at most {maximum}"
            emit(constraint + "; enter numbers, ids, or labels separated by commas.")
            while True:
                raw = read_line("Selections: ")
                aborted = _abort_response(raw, input_type)
                if aborted is not None:
                    return aborted
                tokens = [token.strip() for token in raw.split(",") if token.strip()]
                selected: list[str] = []
                invalid: list[str] = []
                for token in tokens:
                    option = _option_for_token(token, options)
                    if option is None:
                        invalid.append(token)
                    elif option["id"] not in selected:
                        selected.append(option["id"])
                if invalid:
                    emit("Unknown or ambiguous selection: " + ", ".join(invalid))
                    continue
                if len(selected) < minimum or (maximum and len(selected) > maximum):
                    emit(f"Select between {minimum} and {maximum or 'any number of'} options.")
                    continue
                return TerminalHitlResponse(
                    input_type,
                    {"type": "multi_choice", "selected_ids": selected},
                )

        allow_other = input_type == "choice" and schema.get("allow_other") is True
        if allow_other:
            emit("You may also type a free-form value not listed above.")
        while True:
            raw = read_line("Selection: ")
            aborted = _abort_response(raw, input_type)
            if aborted is not None:
                return aborted
            option = _option_for_token(raw, options)
            if option is None:
                if allow_other and raw.strip():
                    return TerminalHitlResponse(
                        input_type,
                        {"type": "choice", "selected_id": "other", "other_value": raw.strip()},
                    )
                emit("Choose a listed number, id, or label.")
                continue
            value: dict[str, Any] = {"type": "choice", "selected_id": option["id"]}
            if option["requires_input"]:
                while True:
                    extra = read_line(f"Input for {option['label']}: ")
                    aborted = _abort_response(extra, input_type)
                    if aborted is not None:
                        return aborted
                    if extra.strip():
                        value["other_value"] = extra.strip()
                        break
                    emit("Additional input is required for that option.")
            return TerminalHitlResponse(input_type, value)

    if input_type == "confirmation":
        confirm_label = str(schema.get("confirm_label") or "Confirm")
        deny_label = str(schema.get("deny_label") or "Deny")
        if schema.get("destructive") is True:
            emit("WARNING: this confirmation authorizes a destructive action.")
        emit(f"  [1] {confirm_label}\n  [2] {deny_label}")
        while True:
            raw = read_line("Decision: ")
            aborted = _abort_response(raw, input_type)
            if aborted is not None:
                return aborted
            folded = raw.strip().casefold()
            if folded in {"1", "y", "yes", "confirm", "approve", confirm_label.casefold()}:
                return TerminalHitlResponse(
                    input_type, {"type": "confirmation", "confirmed": True}
                )
            if folded in {"2", "n", "no", "deny", "reject", deny_label.casefold()}:
                return TerminalHitlResponse(
                    input_type, {"type": "confirmation", "confirmed": False}
                )
            emit("Choose the confirm or deny option.")

    if input_type == "external_action":
        instructions = str(schema.get("instructions") or "Complete the requested external action.")
        done_label = str(schema.get("done_label") or "I've completed this")
        emit("Instructions: " + instructions)
        while True:
            raw = read_line(f"[d] {done_label}: ")
            aborted = _abort_response(raw, input_type)
            if aborted is not None:
                return aborted
            if raw.strip().casefold() in {"d", "done", "completed", done_label.casefold()}:
                guidance = read_line("Optional guidance for the resumed run (Enter to skip): ").strip()
                aborted = _abort_response(guidance, input_type)
                if aborted is not None:
                    return aborted
                value: dict[str, Any] = {"type": "external_action_completed"}
                if guidance:
                    value["guidance"] = guidance
                return TerminalHitlResponse(input_type, value)
            emit("Confirm completion only after the external action is finished.")

    if input_type == "file_path":
        multiple = schema.get("multiple") is True
        file_filter = str(schema.get("filter") or "").strip()
        if file_filter:
            emit(f"Expected file type: {file_filter}")
        if not multiple:
            while True:
                raw = read_line("File path: ")
                aborted = _abort_response(raw, input_type)
                if aborted is not None:
                    return aborted
                if raw.strip():
                    return TerminalHitlResponse(
                        input_type, {"type": "file_path", "paths": [raw.strip()]}
                    )
                emit("A file path is required.")
        emit("Enter one file path per line. Type .done when finished.")
        paths: list[str] = []
        while True:
            raw = read_line("Path: ")
            aborted = _abort_response(raw, input_type)
            if aborted is not None:
                return aborted
            if raw.strip() == ".done":
                if paths:
                    return TerminalHitlResponse(
                        input_type, {"type": "file_path", "paths": paths}
                    )
                emit("At least one file path is required.")
                continue
            if raw.strip():
                paths.append(raw.strip())

    if input_type == "diff_approval":
        files = schema.get("files") if isinstance(schema.get("files"), list) else []
        rationale = str(schema.get("rationale") or "").strip()
        if rationale:
            emit("Rationale: " + rationale)
        approval_id = str(
            schema.get("proposal_id") or schema.get("transaction_id") or ""
        ).strip()
        if approval_id:
            emit("Staged change id: " + approval_id)
        emit("Staged files:")
        valid_files: list[dict[str, Any]] = []
        for index, row in enumerate(files, start=1):
            if not isinstance(row, dict) or not str(row.get("path") or "").strip():
                continue
            valid_files.append(row)
            path = str(row["path"])
            emit(
                f"  [{len(valid_files)}] {row.get('status') or 'M'} {path} "
                f"(+{row.get('additions') or 0}/-{row.get('deletions') or 0})"
            )
            diff = str(row.get("unified_diff") or "")
            if diff:
                emit(diff)
        while True:
            raw = read_line("[a]pply all, apply [s]elected files, or [r]eject: ")
            aborted = _abort_response(raw, input_type)
            if aborted is not None:
                return aborted
            folded = raw.strip().casefold()
            if folded in {"a", "apply", "apply all"}:
                if not valid_files:
                    emit("No valid files were supplied; this request can only be rejected or aborted.")
                    continue
                return TerminalHitlResponse(
                    input_type, {"type": "choice", "selected_id": "apply"}
                )
            if folded in {"r", "reject", "deny"}:
                return TerminalHitlResponse(
                    input_type, {"type": "choice", "selected_id": "reject"}
                )
            if folded in {"s", "select", "selected"}:
                if not valid_files:
                    emit("No valid files were supplied for partial selection.")
                    continue
                selection = read_line("File numbers or exact paths, comma-separated: ")
                aborted = _abort_response(selection, input_type)
                if aborted is not None:
                    return aborted
                selected_paths: list[str] = []
                invalid: list[str] = []
                for token in [part.strip() for part in selection.split(",") if part.strip()]:
                    if token.isdigit() and 1 <= int(token) <= len(valid_files):
                        path = str(valid_files[int(token) - 1]["path"])
                    else:
                        matches = [str(row["path"]) for row in valid_files if str(row["path"]) == token]
                        if len(matches) != 1:
                            invalid.append(token)
                            continue
                        path = matches[0]
                    if path not in selected_paths:
                        selected_paths.append(path)
                if invalid or not selected_paths:
                    emit("Unknown file selection: " + ", ".join(invalid or ["(empty)"]))
                    continue
                return TerminalHitlResponse(
                    input_type,
                    {"type": "choice", "selected_id": "apply"},
                    tuple(selected_paths),
                )
            emit("Choose apply all, selected files, or reject.")

    raise EvalFailure(f"terminal HITL renderer did not handle input_type: {input_type}")


def answer_for_question(
    mode: str, case: dict[str, Any], question: dict[str, Any]
) -> TerminalHitlResponse:
    options = question.get("options") if isinstance(question.get("options"), list) else []
    input_type = "choice" if options else "text"
    snippets = question.get("context_snippets")
    snippets = snippets if isinstance(snippets, list) else []
    request = {
        "input_type": input_type,
        "source": "clarification",
        "prompt": question_prompt(question),
        "hint": "\n".join(str(value) for value in snippets),
        "input_schema": {"options": options},
    }
    if mode == "fixtures":
        answer = fixture_answer(case, question)
        print(f"  fixture answer: {answer}")
        if options:
            option = _option_for_token(answer, _terminal_options(request, request["input_schema"]))
            if option is None and case.get("ad_hoc_prompt") is True:
                # An arbitrary prompt cannot predict model-authored choice ids.
                # This lane approves only a disposable plan and never executes
                # it, so unattended mode may select the first offered planning
                # clarification while preserving the choice in report evidence.
                normalized_options = _terminal_options(request, request["input_schema"])
                option = normalized_options[0] if normalized_options else None
                if option is not None:
                    print(
                        "  ad-hoc choice answer did not match; selecting first "
                        f"planning option: {option['id']}"
                    )
            if option is None:
                raise EvalFailure(
                    f"fixture answer {answer!r} does not match an option for {question_prompt(question)!r}"
                )
            return TerminalHitlResponse(
                "choice", {"type": "choice", "selected_id": option["id"]}
            )
        return TerminalHitlResponse("text", {"type": "text", "value": answer})
    return terminal_hitl_response(request)


def approval_action(mode: str, case: dict[str, Any], approval_index: int) -> str:
    if mode == "fixtures":
        sequence = case.get("approval_sequence") or ["approve"]
        action = str(sequence[min(approval_index, len(sequence) - 1)]).lower()
        if action not in {"approve", "reject"}:
            raise EvalFailure(f"invalid fixture approval action: {action}")
        print(f"  fixture plan decision: {action}")
        return action
    response = terminal_hitl_response(
        {
            "input_type": "confirmation",
            "source": "plan_approval",
            "prompt": "The task plan is ready for review.",
            "input_schema": {"confirm_label": "Approve plan", "deny_label": "Reject and replan"},
        }
    )
    if response.value.get("type") == "aborted":
        raise EvalFailure("operator aborted plan approval")
    return "approve" if response.value.get("confirmed") is True else "reject"


def print_terminal_plan_preview(plan: dict[str, Any]) -> None:
    graph = plan.get("plan_graph")
    rendered = json.dumps(graph, indent=2, sort_keys=True) if graph is not None else "(no graph)"
    if len(rendered) > 6000:
        rendered = rendered[:6000] + "\n… (preview truncated; full plan remains in Plan Inspector)"
    print("\nPlan Inspector draft:\n" + rendered)


def respond_hitl(
    client: Client,
    correlation_id: str,
    source: str,
    task_id: str,
    execution_id: str | None,
    value: dict[str, Any],
    mode: str,
    input_type: str | None = None,
    selected_paths: Iterable[str] = (),
) -> float:
    canonical_input_type = input_type or str(value.get("type") or "text")
    body: dict[str, Any] = {
        "source": source,
        "input_type": canonical_input_type,
        "value": value,
        "channel": "terminal_live_eval" if mode == "terminal" else "fixture_live_eval",
        "task_id": task_id,
    }
    selected_paths = [str(path) for path in selected_paths if str(path).strip()]
    if selected_paths:
        body["selected_paths"] = selected_paths
    if execution_id:
        body["execution_id"] = execution_id
    payload, latency, _ = client.post_json(
        f"/api/magician/v2/hitl/{quote(correlation_id)}/respond",
        body,
        expected=(200, 409),
    )
    if isinstance(payload, dict) and payload.get("accepted") is False:
        reason = payload.get("reason")
        if reason != "already_resolved":
            raise EvalFailure(f"HITL response was not accepted: {payload}")
    return latency


def query_llm_calls(
    client: Client,
    task_id: str,
    execution_ids: list[str],
    started_at_ms: int,
    ended_at_ms: int,
) -> tuple[list[dict[str, Any]], float]:
    safe_task = re.sub(r"[^A-Za-z0-9_.:-]", "", task_id)
    safe_execs = [re.sub(r"[^A-Za-z0-9_.:-]", "", value) for value in execution_ids]
    identities = [f"task_id = '{safe_task}'"]
    identities.extend(f"execution_id = '{value}'" for value in safe_execs if value)
    started_window_ms = started_at_ms - 2000
    ended_window_ms = ended_at_ms + 60000
    sql = (
        "SELECT timestamp_ms, operation, profile, provider, model, success, "
        "input_tokens, output_tokens, reasoning_tokens, queue_wait_ms, "
        "local_prep_ms, provider_execution_ms, latency_ms, cost_usd, "
        "task_id, execution_id, iteration_id, prompt_projection_mode, trace_id FROM llm_calls "
        f"WHERE timestamp_ms >= {started_window_ms} "
        f"AND timestamp_ms <= {ended_window_ms} "
        f"AND ({' OR '.join(identities)}) ORDER BY timestamp_ms"
    )
    # Read the fact registry, not /v2/analytics/llm_calls/query. The latter
    # serves a legacy-compatibility projection with a fixed column list that
    # has never carried the dispatch-timing breakdown, so asking it for
    # queue_wait_ms/local_prep_ms/provider_execution_ms fails the whole case
    # with a DuckDB binder error rather than returning nulls. The fact registry
    # serves the canonical relation, where those columns are populated — and
    # `llm_queue_wait_ms_p95` is a REQUIRED metric that the runtime-performance
    # eval derives from the rows this returns.
    status, payload, latency = client.request(
        "POST",
        "/api/magician/v2/analytics/llm/facts/query",
        {
            "sql": sql,
            "from_ms": started_window_ms,
            "to_ms": ended_window_ms,
            "limit": 1000,
        },
    )
    if status != 200:
        raise EvalFailure(f"LLM call observability query failed with HTTP {status}: {payload}")
    data = payload.get("data") if isinstance(payload, dict) else None
    rows = data.get("rows") if isinstance(data, dict) else None
    if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
        raise EvalFailure(f"LLM call observability returned invalid payload: {payload}")
    return [{str(name): value for name, value in row.items()} for row in rows], latency


def llm_routing_gates(
    case_id: str,
    calls: list[dict[str, Any]],
    config: RoutingConfig,
    latency_ms: float | None,
) -> list[Gate]:
    preplan = [call for call in calls if call.get("operation") in PREPLAN_OPERATIONS]
    mismatches: list[str] = []
    for call in preplan:
        operation = str(call.get("operation"))
        profile = str(call.get("profile") or "")
        if profile not in allowed_profile_chain(config, operation):
            mismatches.append(f"{operation}:{profile}")
            continue
        expected = config.profiles.get(profile)
        if expected and expected.provider and str(call.get("provider")) != expected.provider:
            mismatches.append(f"{operation}:{profile}:provider={call.get('provider')}")
        if expected and expected.model and str(call.get("model")) != expected.model:
            mismatches.append(f"{operation}:{profile}:model={call.get('model')}")
    observed_ops = sorted({str(call.get("operation")) for call in preplan})
    return [
        Gate(
            "llm.task_execution_linkage",
            bool(calls),
            f"linked_calls={len(calls)}",
            case_id,
            latency_ms,
        ),
        Gate(
            "llm.preplan_operations_observed",
            bool(preplan) and "query_analysis" in observed_ops,
            f"operations={observed_ops}",
            case_id,
        ),
        Gate(
            "llm.current_profile_mapping_honored",
            bool(preplan) and not mismatches,
            f"mismatches={mismatches or 'none'}",
            case_id,
        ),
        Gate(
            "llm.calls_succeeded",
            bool(preplan) and all(call.get("success") is not False for call in preplan),
            f"failed={sum(1 for call in preplan if call.get('success') is False)}",
            case_id,
        ),
    ]


def run_case(
    client: Client,
    case: dict[str, Any],
    mode: str,
    timeout_secs: float,
    projection_timeout_secs: float,
    config: RoutingConfig,
    run_suffix: str,
    delete_tasks: bool = False,
) -> tuple[CaseResult, list[Gate]]:
    case_id = f"{case['id']}{run_suffix}"
    result = CaseResult(case_id=case_id, input_prompt=str(case["description"]))
    gates: list[Gate] = []
    started = time.perf_counter()
    started_at_ms = int(time.time() * 1000)
    task_id = ""
    try:
        title = f"[live eval] {case.get('title', case_id)} {run_suffix}".strip()
        payload, create_latency, _ = client.post_json(
            "/api/magician/v3/tasks",
            {
                "title": title,
                "description": str(case["description"]),
                "agent_id": str(case.get("agent_id") or "personal-assistant"),
                "created_by": "preplan-live-eval",
                "approved": True,
                "tags": [
                    {"id": "live-eval", "name": "live-eval"},
                    {"id": "preplan", "name": "preplan"},
                ],
            },
            expected=(201,),
        )
        task_id = task_id_from_response(payload)
        result.task_id = task_id
        gates.append(Gate("task.created", True, f"task={task_id}", case_id, create_latency))

        payload, start_latency, _ = client.post_json(
            f"/api/magician/v3/tasks/{quote(task_id)}/plan", expected=(202,)
        )
        initial_plan = plan_record(payload)
        execution_id = str(initial_plan.get("planning_execution_id") or "") or None
        if execution_id:
            result.planning_execution_ids.append(execution_id)
        gates.append(
            Gate(
                "planning.started",
                str(initial_plan.get("status")) == "planning" and bool(execution_id),
                f"status={initial_plan.get('status')} execution={execution_id}",
                case_id,
                start_latency,
            )
        )

        handled_questions: set[str] = set()
        approval_index = 0
        deadline = time.monotonic() + timeout_secs
        next_status_report = time.monotonic() + 15.0
        while time.monotonic() < deadline:
            plan_payload, _ = poll_plan(client, task_id)
            plan = plan_record(plan_payload)
            if isinstance(plan.get("plan_graph"), dict):
                # Keep the exact public-API graph rendered by Plan Inspector.
                # A rejected draft is replaced when its replanned draft arrives.
                result.plan_graph = plan["plan_graph"]
            status = str(plan.get("status") or "unknown")
            if time.monotonic() >= next_status_report:
                remaining = max(0.0, deadline - time.monotonic())
                print(
                    f"[{case_id}] waiting: plan_status={status} "
                    f"remaining_deadline_secs={remaining:.0f}",
                    flush=True,
                )
                next_status_report = time.monotonic() + 15.0
            execution_id = str(plan.get("planning_execution_id") or "") or execution_id
            if execution_id and execution_id not in result.planning_execution_ids:
                result.planning_execution_ids.append(execution_id)
            pending = plan.get("pending_questions")
            questions = [q for q in pending if isinstance(q, dict)] if isinstance(pending, list) else []

            unanswered = [q for q in questions if question_id(q) not in handled_questions]
            if unanswered:
                question = unanswered[0]
                correlation_id = question_id(question)
                prompt = question_prompt(question)
                print(f"[{case_id}] clarification: {prompt}")
                observation, projection_gates = observe_pending_hitl(
                    client,
                    case_id,
                    task_id,
                    execution_id,
                    correlation_id,
                    "clarification",
                    prompt,
                    projection_timeout_secs,
                )
                result.hitl.append(observation)
                gates.extend(projection_gates)
                answer = answer_for_question(mode, case, question)
                latency = respond_hitl(
                    client,
                    correlation_id,
                    "clarification",
                    task_id,
                    execution_id,
                    answer.value,
                    mode,
                    answer.input_type,
                    answer.selected_paths,
                )
                gates.append(
                    Gate(
                        "clarification.response_accepted",
                        True,
                        f"correlation={correlation_id}",
                        case_id,
                        latency,
                    )
                )
                gates.extend(
                    observe_resolved_hitl(
                        client,
                        case_id,
                        observation,
                        started_at_ms,
                        projection_timeout_secs,
                    )
                )
                handled_questions.add(correlation_id)
                result.clarification_count += 1
                continue

            if status == "draft":
                correlation_id = str(plan.get("plan_id") or "")
                if not correlation_id:
                    raise EvalFailure("draft plan has no immutable plan_id")
                print(f"[{case_id}] plan draft ready: {correlation_id}")
                observation, projection_gates = observe_pending_hitl(
                    client,
                    case_id,
                    task_id,
                    execution_id,
                    correlation_id,
                    "plan_approval",
                    "Task plan is ready for review",
                    projection_timeout_secs,
                )
                result.hitl.append(observation)
                gates.extend(projection_gates)
                if mode == "terminal":
                    print_terminal_plan_preview(plan)
                action = approval_action(mode, case, approval_index)
                latency = respond_hitl(
                    client,
                    correlation_id,
                    "plan_approval",
                    task_id,
                    execution_id,
                    {"type": "confirmation", "confirmed": action == "approve"},
                    mode,
                )
                gates.append(
                    Gate(
                        "plan_approval.response_accepted",
                        True,
                        f"decision={action} plan={correlation_id}",
                        case_id,
                        latency,
                    )
                )
                gates.extend(
                    observe_resolved_hitl(
                        client,
                        case_id,
                        observation,
                        started_at_ms,
                        projection_timeout_secs,
                    )
                )
                result.approval_count += 1
                approval_index += 1
                if action == "approve":
                    _, _ = wait_until(
                        f"plan {correlation_id} to become approved",
                        projection_timeout_secs,
                        lambda: _plan_status_probe(client, task_id, "approved"),
                    )
                    result.final_status = "approved"
                    break
                _, _ = wait_until(
                    f"plan {correlation_id} to become rejected",
                    projection_timeout_secs,
                    lambda: _plan_status_probe(client, task_id, "rejected"),
                )
                payload, _, _ = client.post_json(
                    f"/api/magician/v3/tasks/{quote(task_id)}/plan/replan",
                    expected=(202,),
                )
                replanned = plan_record(payload)
                new_execution = str(replanned.get("planning_execution_id") or "")
                if new_execution and new_execution not in result.planning_execution_ids:
                    result.planning_execution_ids.append(new_execution)
                # Question ids are scoped to a planning execution in practice,
                # but an elicitor is allowed to deterministically reissue an
                # unresolved semantic question. A replacement plan must be
                # evaluated as a fresh HITL lifecycle even when that id repeats.
                handled_questions.clear()
                result.replan_count += 1
                continue

            if status == "failed":
                raise EvalFailure(f"planning failed: {plan.get('error')}")
            if status == "approved":
                result.final_status = status
                break
            if status == "rejected":
                raise EvalFailure("plan remained rejected without an evaluator replan decision")
            time.sleep(POLL_INTERVAL_SECS)
        else:
            raise EvalFailure(f"case exceeded {timeout_secs:.0f}s planning deadline")

        min_clarifications = int(case.get("expected_min_clarifications", 0))
        gates.extend(
            [
                Gate(
                    "clarification.minimum_met",
                    result.clarification_count >= min_clarifications,
                    f"observed={result.clarification_count} required={min_clarifications}",
                    case_id,
                ),
                Gate(
                    "plan.final_status",
                    result.final_status == "approved",
                    f"status={result.final_status}",
                    case_id,
                ),
                Gate(
                    "plan.rejection_replan_coverage",
                    result.replan_count >= int(case.get("expected_min_replans", 0)),
                    f"observed={result.replan_count} required={case.get('expected_min_replans', 0)}",
                    case_id,
                ),
                Gate(
                    "plan.output_graph_captured",
                    isinstance(result.plan_graph, dict)
                    and isinstance(result.plan_graph.get("steps"), list)
                    and bool(result.plan_graph["steps"]),
                    (
                        f"steps={len(result.plan_graph.get('steps', []))} "
                        f"edges={len(result.plan_graph.get('edges', []))}"
                        if isinstance(result.plan_graph, dict)
                        else "graph=missing"
                    ),
                    case_id,
                ),
            ]
        )

        # LLM facts flush asynchronously. Give the normal recorder a bounded
        # opportunity before declaring a linkage/routing regression.
        ended_at_ms = int(time.time() * 1000)
        calls: list[dict[str, Any]] = []
        query_latency = 0.0
        def llm_probe() -> tuple[bool, Any, float]:
            nonlocal calls, query_latency
            calls, latency = query_llm_calls(
                client, task_id, result.planning_execution_ids, started_at_ms, ended_at_ms
            )
            query_latency += latency
            relevant = [c for c in calls if c.get("operation") in PREPLAN_OPERATIONS]
            return bool(relevant), {"linked": len(calls), "preplan": len(relevant)}, latency

        try:
            wait_until("linked pre-plan LLM facts", min(45.0, projection_timeout_secs), llm_probe)
        except EvalFailure:
            # Preserve the zero-row evidence; the linkage gate below owns the
            # failure and produces a useful report instead of losing the case.
            pass
        result.llm_calls = calls
        gates.extend(llm_routing_gates(case_id, calls, config, query_latency))
    except Exception as error:  # report-first live lane: retain all evidence
        result.error = str(error)
        result.final_status = result.final_status if result.final_status != "unknown" else "failed"
        gates.append(Gate("case.completed", False, str(error), case_id))
    finally:
        result.duration_ms = (time.perf_counter() - started) * 1000
        if task_id and not delete_tasks:
            # **Kept by default.** The task carries the run's decision events —
            # the only record of WHY a run behaved as it did, and not
            # reconstructable from the report. `--delete-tasks` restores the old
            # teardown for loop runs that must not grow the store.
            gates.append(
                Gate(
                    "cleanup.task_retained",
                    True,
                    f"task={task_id} kept for inspection (--delete-tasks to remove)",
                    case_id,
                )
            )
        elif task_id:
            try:
                gates.extend(cleanup_task(client, case_id, task_id))
            except Exception as cleanup_error:
                # Preserve the primary lifecycle evidence even when a crashed
                # or unreachable backend prevents best-effort teardown.
                gates.append(
                    Gate(
                        "cleanup.task_removed",
                        False,
                        f"cleanup failed: {cleanup_error}",
                        case_id,
                    )
                )
    return result, gates


def _plan_status_probe(client: Client, task_id: str, expected: str) -> tuple[bool, Any, float]:
    payload, latency = poll_plan(client, task_id)
    status = str(plan_record(payload).get("status") or "unknown")
    return status == expected, {"status": status}, latency


def load_cases(path: Path) -> list[dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    cases = payload.get("cases") if isinstance(payload, dict) else None
    if not isinstance(cases, list) or not cases:
        raise EvalFailure(f"fixture file has no cases: {path}")
    normalized = [case for case in cases if isinstance(case, dict)]
    for case in normalized:
        if not case.get("id") or not case.get("description"):
            raise EvalFailure(f"fixture case requires id and description: {case}")
    return normalized


def ad_hoc_prompt_case(prompt: str) -> dict[str, Any]:
    normalized = prompt.strip()
    if not normalized:
        raise EvalFailure("--prompt must contain a non-empty planning request")
    if len(normalized) > MAX_AD_HOC_PROMPT_CHARS:
        raise EvalFailure(
            f"--prompt exceeds the {MAX_AD_HOC_PROMPT_CHARS}-character safety limit"
        )
    one_line = " ".join(normalized.split())
    title = one_line[:76] + ("…" if len(one_line) > 76 else "")
    return {
        "id": "ad_hoc_prompt",
        "title": title,
        "description": normalized,
        "expected_min_clarifications": 0,
        "expected_min_replans": 0,
        "approval_sequence": ["approve"],
        "answer_rules": [],
        "default_answer": (
            "Use reasonable, reversible assumptions and state every remaining "
            "assumption explicitly in the plan."
        ),
        "ad_hoc_prompt": True,
    }


def resolve_cases(fixtures: Path | None, prompt: str | None) -> list[dict[str, Any]]:
    if prompt is not None and fixtures is not None:
        raise EvalFailure("--prompt and --fixtures are alternative case sources")
    if prompt is not None:
        return [ad_hoc_prompt_case(prompt)]
    return load_cases(fixtures or DEFAULT_FIXTURES)


def synthetic_self_test(config_path: Path) -> tuple[list[CaseResult], list[Gate], RoutingConfig]:
    config = parse_routing_config(config_path)
    task_id = "task-self-test"
    correlation = "question-self-test"
    attention = {
        "requests": [
            {
                "id": f"attention:{correlation}",
                "task_id": task_id,
                "metadata": {
                    "correlation_id": correlation,
                    "source": "clarification",
                    "execution_id": "planexec-self-test",
                },
            }
        ],
        "approvals": [],
        "escalations": [],
    }
    matched = find_attention(attention, correlation, task_id)
    sample_operation = "query_analysis"
    profile = config.operation_mapping[sample_operation]
    definition = config.profiles[profile]
    calls = [
        {
            "operation": sample_operation,
            "profile": profile,
            "provider": definition.provider,
            "model": definition.model,
            "success": True,
        }
    ]
    event = {
        "event_type": "hitl.resolved",
        "data": {"correlation_id": correlation},
    }
    synthetic_graph = {
        "steps": [
            {
                "id": "understand",
                "task": "Understand the request",
                "tool": None,
                "confidence": 0.96,
                "providing_agent_id": "personal-assistant",
            },
            {
                "id": "analyze",
                "task": "Analyze constraints and resolve inputs",
                "tool": "query_analysis",
                "confidence": 0.91,
            },
            {
                "id": "compose",
                "task": "Compose the executable plan",
                "tool": "task_decomposition",
                "confidence": 0.88,
            },
            {
                "id": "review",
                "task": "Present the plan for review",
                "tool": None,
                "confidence": 0.94,
            },
        ],
        "edges": [
            {"from": "understand", "to": "analyze", "reason": "request context"},
            {"from": "analyze", "to": "compose", "reason": "resolved constraints"},
            {"from": "compose", "to": "review", "reason": "draft ready"},
        ],
        "unresolved_inputs": [],
        "confidence": 0.92,
        "provenance": {
            "strategy": "synthetic_contract_fixture",
            "generator": None,
            "notes": "Provider-free renderer and schema fixture; not model output",
        },
    }
    synthetic_routing_gates = llm_routing_gates(
        "self_test", calls, config, None
    )
    synthetic_routing_gates = [
        Gate(
            "self_test.routing_fixture_present",
            bool(calls),
            f"synthetic_fixture_calls={len(calls)}",
        ),
        *[
            Gate(
                "self_test." + gate.name + ".contract",
                gate.passed,
                "synthetic routing fixture: " + gate.detail,
                gate.case_id,
                None,
            )
            for gate in synthetic_routing_gates
            if gate.name != "llm.task_execution_linkage"
        ],
    ]
    gates = [
        Gate("self_test.attention_identity", matched is not None, "typed alias match"),
        Gate(
            "self_test.event_pairing",
            event_matches(event, correlation, "hitl.resolved"),
            "canonical resolved event match",
        ),
        Gate(
            "self_test.fixture_answer",
            fixture_answer(
                {
                    "id": "self",
                    "answer_rules": [{"contains_any": ["date"], "answer": "2026-09-15"}],
                    "default_answer": "default",
                },
                {"question_text": "What launch date?"},
            )
            == "2026-09-15",
            "keyword rule",
        ),
        Gate(
            "self_test.plan_graph_fixture",
            bool(synthetic_graph["steps"]) and bool(synthetic_graph["edges"]),
            "synthetic PlanGraph fixture is renderable",
        ),
        *synthetic_routing_gates,
    ]
    return [
        CaseResult(
            case_id="self_test",
            input_prompt="Synthetic provider-free PlanGraph renderer contract",
            final_status="synthetic_contract",
            plan_graph=synthetic_graph,
        )
    ], gates, config


def report_payload(
    mode: str,
    config: RoutingConfig,
    results: list[CaseResult],
    gates: list[Gate],
) -> dict[str, Any]:
    calls = [call for result in results for call in result.llm_calls]
    runtime_metrics_available = mode in {"fixtures", "terminal"}
    measured_durations = [
        result.duration_ms for result in results if result.duration_ms is not None
    ]
    measured_api_latencies = [
        gate.latency_ms for gate in gates if gate.latency_ms is not None
    ]
    return {
        "schema_version": 2,
        "evaluator": "preplan-flow-live",
        "mode": mode,
        "measurement_kind": "runtime" if runtime_metrics_available else "synthetic_contract",
        "runtime_metrics_available": runtime_metrics_available,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "passed": bool(gates) and all(gate.passed for gate in gates),
        "routing_config": {
            "path": config.path,
            "operation_mapping": config.operation_mapping,
        },
        "summary": {
            "cases": len(results),
            "clarifications": sum(result.clarification_count for result in results),
            "plan_approvals": sum(result.approval_count for result in results),
            "replans": sum(result.replan_count for result in results),
            "plan_panel_hitl": sum(
                1 for result in results for observation in result.hitl
                if observation.plan_panel_visible
            ),
            "attention_hitl": sum(
                1 for result in results for observation in result.hitl
                if observation.attention_visible
            ),
            "resolved_hitl": sum(
                1 for result in results for observation in result.hitl
                if observation.plan_panel_cleared
                and observation.attention_cleared
                and observation.resolved_event_visible
            ),
            "gates": len(gates),
            "passed_gates": sum(1 for gate in gates if gate.passed),
            "duration_ms": sum(measured_durations) if runtime_metrics_available else None,
            "api_latency_ms": (
                sum(measured_api_latencies) if runtime_metrics_available else None
            ),
            "llm_calls": len(calls) if runtime_metrics_available else None,
            "input_tokens": (
                sum(int(call.get("input_tokens") or 0) for call in calls)
                if runtime_metrics_available else None
            ),
            "output_tokens": (
                sum(int(call.get("output_tokens") or 0) for call in calls)
                if runtime_metrics_available else None
            ),
            "reasoning_tokens": (
                sum(int(call.get("reasoning_tokens") or 0) for call in calls)
                if runtime_metrics_available else None
            ),
            "cost_usd": (
                sum(float(call.get("cost_usd") or 0.0) for call in calls)
                if runtime_metrics_available else None
            ),
        },
        "gates": [asdict(gate) for gate in gates],
        "cases": [asdict(result) for result in results],
    }


def _plan_graph_parts(
    graph: dict[str, Any] | None,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Return renderable, identity-safe PlanGraph rows without mutating evidence."""
    if not isinstance(graph, dict):
        return [], []
    raw_steps = graph.get("steps")
    raw_edges = graph.get("edges")
    if not isinstance(raw_steps, list):
        return [], []
    steps: list[dict[str, Any]] = []
    seen: set[str] = set()
    for row in raw_steps:
        if not isinstance(row, dict):
            continue
        step_id = str(row.get("id") or "").strip()
        if not step_id or step_id in seen:
            continue
        seen.add(step_id)
        steps.append(row)
    edges = [
        row for row in (raw_edges if isinstance(raw_edges, list) else [])
        if isinstance(row, dict)
        and str(row.get("from") or "") in seen
        and str(row.get("to") or "") in seen
    ]
    return steps, edges


def _plan_graph_levels(
    steps: list[dict[str, Any]], edges: list[dict[str, Any]]
) -> dict[str, int]:
    """Stable Kahn layout; cyclic leftovers occupy a final diagnostic column."""
    ids = [str(step["id"]) for step in steps]
    indegree = {step_id: 0 for step_id in ids}
    children = {step_id: [] for step_id in ids}
    for edge in edges:
        source, target = str(edge["from"]), str(edge["to"])
        indegree[target] += 1
        children[source].append(target)
    levels = {step_id: 0 for step_id in ids}
    queue = [step_id for step_id in ids if indegree[step_id] == 0]
    visited: set[str] = set()
    while queue:
        step_id = queue.pop(0)
        visited.add(step_id)
        for child in children[step_id]:
            levels[child] = max(levels[child], levels[step_id] + 1)
            indegree[child] -= 1
            if indegree[child] == 0:
                queue.append(child)
    if len(visited) != len(ids):
        cycle_level = max((levels[item] for item in visited), default=-1) + 1
        for step_id in ids:
            if step_id not in visited:
                levels[step_id] = cycle_level
    return levels


def _svg_text_lines(value: Any, width: int = 30, limit: int = 3) -> list[str]:
    text = " ".join(str(value or "Untitled step").split())
    lines = textwrap.wrap(text, width=width, break_long_words=False, break_on_hyphens=False)
    if not lines:
        return ["Untitled step"]
    if len(lines) > limit:
        lines = lines[:limit]
        lines[-1] = lines[-1][: max(1, width - 1)].rstrip() + "…"
    return lines


def render_plan_graph_html(
    result: CaseResult,
    graph_index: int,
    synthetic: bool,
) -> str:
    prompt_html = (
        '<div class="plan-prompt"><strong>Planning request</strong><span>'
        + escape(result.input_prompt)
        + "</span></div>"
        if result.input_prompt else ""
    )
    steps, edges = _plan_graph_parts(result.plan_graph)
    if not steps:
        return (
            f'<article class="plan-output"><h3>{escape(result.case_id)}</h3>'
            f"{prompt_html}"
            '<p class="empty-output">No PlanGraph output was captured.</p></article>'
        )

    levels = _plan_graph_levels(steps, edges)
    columns: dict[int, list[dict[str, Any]]] = {}
    for step in steps:
        columns.setdefault(levels[str(step["id"])], []).append(step)
    node_width, node_height = 240, 116
    column_gap, row_gap, margin = 86, 42, 42
    max_rows = max(len(rows) for rows in columns.values())
    width = margin * 2 + (max(columns) + 1) * node_width + max(columns) * column_gap
    height = margin * 2 + max_rows * node_height + max(0, max_rows - 1) * row_gap
    positions: dict[str, tuple[float, float]] = {}
    for level, rows in sorted(columns.items()):
        content_height = len(rows) * node_height + max(0, len(rows) - 1) * row_gap
        top = margin + (height - 2 * margin - content_height) / 2
        for row_index, step in enumerate(rows):
            positions[str(step["id"])] = (
                margin + level * (node_width + column_gap),
                top + row_index * (node_height + row_gap),
            )

    marker_id = f"plan-arrow-{graph_index}"
    edge_svg: list[str] = []
    for edge in edges:
        source, target = str(edge["from"]), str(edge["to"])
        sx, sy = positions[source]
        tx, ty = positions[target]
        start_x, start_y = sx + node_width, sy + node_height / 2
        end_x, end_y = tx, ty + node_height / 2
        bend = max(26.0, (end_x - start_x) * 0.5)
        path = (
            f"M {start_x:.1f} {start_y:.1f} C {start_x + bend:.1f} {start_y:.1f}, "
            f"{end_x - bend:.1f} {end_y:.1f}, {end_x:.1f} {end_y:.1f}"
        )
        reason = " ".join(str(edge.get("reason") or "dependency").split())
        edge_svg.append(
            f'<path class="plan-edge" d="{path}" marker-end="url(#{marker_id})">'
            f"<title>{escape(reason)}</title></path>"
        )
        if reason and abs(end_y - start_y) < 12:
            edge_svg.append(
                f'<text class="edge-reason" x="{(start_x + end_x) / 2:.1f}" '
                f'y="{start_y - 9:.1f}" text-anchor="middle">'
                f"{escape(reason[:34] + ('…' if len(reason) > 34 else ''))}</text>"
            )

    node_svg: list[str] = []
    incoming = {str(edge["to"]) for edge in edges}
    outgoing = {str(edge["from"]) for edge in edges}
    for step in steps:
        step_id = str(step["id"])
        x, y = positions[step_id]
        try:
            confidence = float(step.get("confidence") or 0.0)
        except (TypeError, ValueError):
            confidence = 0.0
        confidence_class = "high" if confidence >= 0.7 else "medium" if confidence >= 0.4 else "low"
        task_lines = _svg_text_lines(step.get("task"))
        tool = " ".join(str(step.get("tool") or "No tool selected").split())
        agent = " ".join(str(step.get("providing_agent_id") or "").split())
        badges = []
        if step_id not in incoming:
            badges.append("START")
        if step_id not in outgoing:
            badges.append("END")
        node_svg.append(
            f'<g class="plan-node {confidence_class}" transform="translate({x:.1f},{y:.1f})">'
            f'<title>{escape(str(step.get("task") or step_id))}</title>'
            f'<rect class="node-body" width="{node_width}" height="{node_height}" rx="15"/>'
            f'<rect class="node-accent" width="6" height="{node_height}" rx="3"/>'
            f'<text class="node-order" x="18" y="23">{escape(" · ".join(badges) or step_id)}</text>'
            f'<text class="node-confidence" x="{node_width - 16}" y="23" text-anchor="end">{confidence:.0%}</text>'
        )
        for line_index, line in enumerate(task_lines):
            node_svg.append(
                f'<text class="node-title" x="18" y="{48 + line_index * 17}">{escape(line)}</text>'
            )
        node_svg.append(
            f'<text class="node-tool" x="18" y="{node_height - 14}">'
            f"{escape('◆ ' + tool[:31] + ('…' if len(tool) > 31 else ''))}</text>"
        )
        if agent:
            node_svg.append(
                f'<text class="node-agent" x="{node_width - 16}" y="{node_height - 14}" '
                f'text-anchor="end">{escape("via " + agent[:20])}</text>'
            )
        node_svg.append("</g>")

    graph = result.plan_graph or {}
    provenance = graph.get("provenance") if isinstance(graph.get("provenance"), dict) else {}
    provenance_text = " · ".join(
        part for part in (
            str(provenance.get("strategy") or "").strip(),
            str(provenance.get("generator") or "").strip(),
        ) if part
    )
    label = "Synthetic renderer fixture" if synthetic else "Captured Plan API output"
    raw_json = escape(json.dumps(graph, indent=2, sort_keys=True))
    # Four-column plans remain legible when fit to the report. Larger graphs
    # retain their natural width and use the same horizontal pan affordance as
    # the app instead of shrinking node text into unreadability.
    svg_width = "100%" if max(columns) <= 3 else str(width)
    svg_height = "auto" if svg_width == "100%" else str(height)
    return f"""<article class="plan-output">
<div class="plan-output-heading"><div><h3>{escape(result.case_id)}</h3><p>{escape(label)}{(' · ' + escape(provenance_text)) if provenance_text else ''}</p></div><span>{len(steps)} steps · {len(edges)} dependencies</span></div>
{prompt_html}
<div class="plan-graph-scroll"><svg class="plan-graph-svg" role="img" aria-label="Plan dependency graph for {escape(result.case_id)}" viewBox="0 0 {width} {height}" width="{svg_width}" height="{svg_height}">
<defs><marker id="{marker_id}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z"/></marker></defs>
{''.join(edge_svg)}{''.join(node_svg)}</svg></div>
<details><summary>Raw PlanGraph JSON</summary><pre>{raw_json}</pre></details>
</article>"""


def write_report(
    output_dir: Path,
    mode: str,
    config: RoutingConfig,
    results: list[CaseResult],
    gates: list[Gate],
) -> dict[str, Any]:
    output_dir.mkdir(parents=True, exist_ok=True)
    payload = report_payload(mode, config, results, gates)
    (output_dir / "report.json").write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    runtime_metrics_available = bool(payload["runtime_metrics_available"])
    report_title = {
        "self-test": "Pre-plan deterministic harness",
        "dry-run": "Pre-plan dry-run validation",
    }.get(mode, "Pre-plan live evaluation")

    def format_latency(value: float | None) -> str:
        return f"{value:.1f}" if value is not None else "—"

    def format_duration(value: float | None) -> str:
        return f"{value / 1000:.1f}s" if value is not None else "—"

    gate_rows = "\n".join(
        "<tr>"
        f"<td>{escape(gate.case_id)}</td>"
        f"<td>{escape(gate.name)}</td>"
        f"<td class='{('pass' if gate.passed else 'fail')}'>{'PASS' if gate.passed else 'FAIL'}</td>"
        f"<td>{escape(gate.detail)}</td>"
        f"<td>{format_latency(gate.latency_ms)}</td>"
        "</tr>"
        for gate in gates
    )
    if runtime_metrics_available:
        case_header = (
            "<th>Case</th><th>Final</th><th>Clarifications</th><th>Decisions</th>"
            "<th>Replans</th><th>LLM calls</th><th>Duration</th><th>Error</th>"
        )
        case_rows = "\n".join(
            "<tr>"
            f"<td>{escape(result.case_id)}</td>"
            f"<td>{escape(result.final_status)}</td>"
            f"<td>{result.clarification_count}</td>"
            f"<td>{result.approval_count}</td>"
            f"<td>{result.replan_count}</td>"
            f"<td>{len(result.llm_calls)}</td>"
            f"<td>{format_duration(result.duration_ms)}</td>"
            f"<td>{escape(result.error or '')}</td>"
            "</tr>"
            for result in results
        )
    else:
        case_header = "<th>Contract case</th><th>Result</th><th>Error</th>"
        case_rows = "\n".join(
            "<tr>"
            f"<td>{escape(result.case_id)}</td>"
            f"<td>{escape(result.final_status)}</td>"
            f"<td>{escape(result.error or '')}</td>"
            "</tr>"
            for result in results
        )
    routing_rows = "\n".join(
        "<tr>"
        f"<td>{escape(operation)}</td>"
        f"<td>{escape(profile)}</td>"
        f"<td>{escape(config.profiles.get(profile, ConfigProfile()).provider or '')}</td>"
        f"<td>{escape(config.profiles.get(profile, ConfigProfile()).model or '')}</td>"
        "</tr>"
        for operation, profile in sorted(config.operation_mapping.items())
    )
    graph_views = "\n".join(
        render_plan_graph_html(
            result,
            graph_index,
            synthetic=not runtime_metrics_available,
        )
        for graph_index, result in enumerate(results, start=1)
    )
    summary = payload["summary"]
    if runtime_metrics_available:
        cards = f"""
<div class="card"><strong>{summary['cases']}</strong><br>cases</div>
<div class="card"><strong>{summary['clarifications']}</strong><br>clarifications</div>
<div class="card"><strong>{summary['plan_approvals']}</strong><br>plan decisions</div>
<div class="card"><strong>{summary['replans']}</strong><br>replans</div>
<div class="card"><strong>{summary['plan_panel_hitl']}</strong><br>Plan-panel HITL</div>
<div class="card"><strong>{summary['attention_hitl']}</strong><br>Attention HITL</div>
<div class="card"><strong>{summary['resolved_hitl']}</strong><br>fully resolved HITL</div>
<div class="card"><strong>{summary['llm_calls']}</strong><br>linked LLM calls</div>
<div class="card"><strong>{format_duration(summary['duration_ms'])}</strong><br>total duration</div>
<div class="card"><strong>{summary['api_latency_ms']:.1f} ms</strong><br>observed API time</div>
<div class="card"><strong>${summary['cost_usd']:.6f}</strong><br>observed cost</div>"""
        measurement_notice = (
            '<p class="notice live"><strong>Measured runtime evaluation.</strong> '
            "Durations, API latency, linked LLM calls, tokens, and cost come from "
            "the exercised backend and its analytics telemetry.</p>"
        )
    else:
        cards = f"""
<div class="card"><strong>{summary['passed_gates']}/{summary['gates']}</strong><br>contract gates passed</div>
<div class="card"><strong>{summary['cases']}</strong><br>synthetic contract cases</div>
<div class="card"><strong>{len(config.operation_mapping)}</strong><br>routing mappings checked</div>"""
        measurement_notice = (
            '<p class="notice"><strong>Provider-free contract harness.</strong> '
            "No backend request, task execution, or LLM call was made. Runtime duration, "
            "API latency, token usage, and cost are intentionally unavailable—not zero. "
            "Run <code>make test-preplan-flow-live-eval</code> for measured data.</p>"
        )
    html = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{escape(report_title)}</title>
<style>
body{{font:14px/1.5 system-ui,-apple-system,sans-serif;margin:32px;background:#10131a;color:#e8edf5}}
h1,h2{{letter-spacing:-.02em}} .meta{{color:#9eabc0}} .cards{{display:flex;gap:12px;flex-wrap:wrap}}
.card{{background:#171c26;border:1px solid #2a3344;border-radius:12px;padding:12px 16px;min-width:120px}}
.notice{{max-width:960px;background:#241f17;border:1px solid #68542d;border-radius:12px;padding:12px 16px;color:#f1d89e}}
.notice.live{{background:#15241d;border-color:#2d6846;color:#b8eccd}}
table{{width:100%;border-collapse:collapse;margin:12px 0 28px;background:#151a23}}
th,td{{text-align:left;vertical-align:top;border-bottom:1px solid #2a3344;padding:9px}}
th{{color:#b8c5d9}} .pass{{color:#63d392;font-weight:700}} .fail{{color:#ff7b86;font-weight:700}}
code{{color:#b8d7ff}} a{{color:#85b8ff}}
.plan-output{{margin:12px 0 28px;background:#151a23;border:1px solid #2a3344;border-radius:16px;overflow:hidden}}
.plan-output-heading{{display:flex;align-items:center;justify-content:space-between;gap:20px;padding:16px 18px;border-bottom:1px solid #2a3344}}
.plan-output-heading h3{{margin:0 0 3px;font-size:17px}} .plan-output-heading p{{margin:0;color:#9eabc0}}
.plan-output-heading>span{{color:#b8c5d9;white-space:nowrap}} .plan-graph-scroll{{overflow:auto;padding:8px;background:radial-gradient(circle at 1px 1px,#293246 1px,transparent 0);background-size:20px 20px}}
.plan-prompt{{display:grid;grid-template-columns:auto 1fr;gap:12px;padding:11px 18px;border-bottom:1px solid #2a3344;background:#171d28}}
.plan-prompt strong{{color:#b8c5d9}} .plan-prompt span{{color:#e8edf5;white-space:pre-wrap}}
.plan-graph-svg{{display:block;margin:auto;min-width:720px}} .plan-edge{{fill:none;stroke:#77a8f8;stroke-width:2.4;opacity:.88}}
.plan-edge+text,.edge-reason{{fill:#9eabc0;font-size:10px;paint-order:stroke;stroke:#151a23;stroke-width:4px;stroke-linejoin:round}}
marker path{{fill:#77a8f8}} .node-body{{fill:#1b2230;stroke:#3a465d;stroke-width:1.5}}
.node-accent{{fill:#63d392}} .plan-node.medium .node-accent{{fill:#e4b65b}} .plan-node.low .node-accent{{fill:#ff7b86}}
.node-order{{fill:#91a2bc;font-size:10px;font-weight:700;letter-spacing:.08em;text-transform:uppercase}}
.node-confidence{{fill:#63d392;font-size:11px;font-weight:700}} .plan-node.medium .node-confidence{{fill:#e4b65b}} .plan-node.low .node-confidence{{fill:#ff7b86}}
.node-title{{fill:#edf3fb;font-size:13px;font-weight:650}} .node-tool{{fill:#9fc5ff;font-size:10px}} .node-agent{{fill:#c7a7ff;font-size:10px}}
.plan-output details{{border-top:1px solid #2a3344;padding:11px 16px}} .plan-output summary{{cursor:pointer;color:#b8c5d9}}
.plan-output pre{{max-height:420px;overflow:auto;white-space:pre-wrap;color:#b8c5d9;font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}}
.empty-output{{padding:0 18px 18px;color:#9eabc0}}
@media(max-width:720px){{body{{margin:18px}}.plan-output-heading{{align-items:flex-start;flex-direction:column}}}}
</style></head><body>
<h1>{'PASS' if payload['passed'] else 'FAIL'} · {escape(report_title)}</h1>
<p class="meta">Mode: {escape(mode)} · Config: <code>{escape(config.path)}</code> · {escape(payload['generated_at'])}</p>
{measurement_notice}
<div class="cards">
{cards}
</div>
<h2>Cases</h2><table><thead><tr>{case_header}</tr></thead><tbody>{case_rows}</tbody></table>
<h2>Plan output graphs</h2>{graph_views}
<h2>Gates</h2><table><thead><tr><th>Case</th><th>Gate</th><th>Status</th><th>Evidence</th><th>Observed API ms</th></tr></thead><tbody>{gate_rows}</tbody></table>
<h2>Current pre-plan routing</h2><table><thead><tr><th>Operation</th><th>Profile</th><th>Provider</th><th>Model</th></tr></thead><tbody>{routing_rows}</tbody></table>
<p><a href="report.json">Raw JSON evidence</a></p>
</body></html>"""
    (output_dir / "report.html").write_text(html, encoding="utf-8")
    return payload


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--api-base-url", default=os.environ.get("PREPLAN_LIVE_API_BASE_URL", DEFAULT_BASE_URL)
    )
    parser.add_argument(
        "--principal", default=os.environ.get("PREPLAN_LIVE_PRINCIPAL", DEFAULT_PRINCIPAL)
    )
    parser.add_argument(
        "--workspace", default=os.environ.get("PREPLAN_LIVE_WORKSPACE", DEFAULT_WORKSPACE)
    )
    parser.add_argument("--config")
    parser.add_argument(
        "--delete-tasks",
        action="store_true",
        help=(
            "Delete each case's task when it finishes. OFF by default: the task "
            "holds the run's decision events, the only record of WHY a run "
            "behaved as it did, and they cannot be reconstructed from the report."
        ),
    )
    case_source = parser.add_mutually_exclusive_group()
    case_source.add_argument("--fixtures", type=Path)
    case_source.add_argument(
        "--prompt",
        help=(
            "run one arbitrary planning request instead of the committed fixture corpus; "
            "also accepted through PREPLAN_LIVE_PROMPT"
        ),
    )
    parser.add_argument(
        "--hitl-mode",
        choices=("terminal", "fixtures"),
        default=os.environ.get("PREPLAN_LIVE_HITL_MODE", "fixtures"),
    )
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument(
        "--timeout-secs",
        type=float,
        default=float(os.environ.get("PREPLAN_LIVE_TIMEOUT_SECS", "1200")),
    )
    parser.add_argument(
        "--projection-timeout-secs",
        type=float,
        default=float(os.environ.get("PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS", "45")),
    )
    parser.add_argument(
        "--http-timeout-secs",
        type=float,
        default=float(os.environ.get("PREPLAN_LIVE_HTTP_TIMEOUT_SECS", "120")),
    )
    parser.add_argument("--output-dir", type=Path, default=REPO_ROOT / "coverage/evals/preplan-flow/latest")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.runs < 1:
        raise EvalFailure("--runs must be positive")
    for name, value in (
        ("--timeout-secs", args.timeout_secs),
        ("--projection-timeout-secs", args.projection_timeout_secs),
        ("--http-timeout-secs", args.http_timeout_secs),
    ):
        if value <= 0:
            raise EvalFailure(f"{name} must be positive")
    config_path = resolve_config_path(args.config)
    if args.self_test:
        if args.prompt is not None or args.fixtures is not None:
            raise EvalFailure("--self-test does not accept --prompt or --fixtures")
        results, gates, config = synthetic_self_test(config_path)
        payload = write_report(args.output_dir, "self-test", config, results, gates)
    elif args.dry_run:
        config = parse_routing_config(config_path)
        prompt = args.prompt if args.prompt is not None else os.environ.get("PREPLAN_LIVE_PROMPT")
        cases = resolve_cases(args.fixtures, prompt)
        results = [
            CaseResult(
                case_id=str(case["id"]),
                input_prompt=str(case["description"]),
                final_status="planned",
            )
            for case in cases
        ]
        gates = [
            Gate("dry_run.fixture_loaded", True, f"cases={len(cases)}"),
            Gate(
                "dry_run.routing_loaded",
                bool(config.operation_mapping),
                f"operations={len(config.operation_mapping)}",
            ),
        ]
        payload = write_report(args.output_dir, "dry-run", config, results, gates)
    else:
        config = parse_routing_config(config_path)
        prompt = args.prompt if args.prompt is not None else os.environ.get("PREPLAN_LIVE_PROMPT")
        cases = resolve_cases(args.fixtures, prompt)
        client = Client(
            args.api_base_url,
            args.principal,
            args.workspace,
            args.http_timeout_secs,
        )
        health_status, health, _ = client.request("GET", "/health")
        if health_status != 200:
            raise EvalFailure(f"runtime health check failed with HTTP {health_status}: {health}")
        results: list[CaseResult] = []
        gates: list[Gate] = []
        for run_index in range(args.runs):
            suffix = "" if args.runs == 1 else f"-run-{run_index + 1}"
            for case in cases:
                result, case_gates = run_case(
                    client,
                    case,
                    args.hitl_mode,
                    args.timeout_secs,
                    args.projection_timeout_secs,
                    config,
                    suffix,
                    args.delete_tasks,
                )
                results.append(result)
                gates.extend(case_gates)
        payload = write_report(args.output_dir, args.hitl_mode, config, results, gates)

    report_path = (args.output_dir / "report.html").resolve()
    run_label = {
        "self-test": "Pre-plan deterministic harness",
        "dry-run": "Pre-plan dry-run validation",
    }.get(str(payload["mode"]), "Pre-plan live evaluation")
    print(f"{run_label}: {'PASS' if payload['passed'] else 'FAIL'}")
    print(f"Report: file://{report_path}")
    return 0 if payload["passed"] else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except EvalFailure as error:
        print(f"pre-plan evaluator failed: {error}", file=sys.stderr)
        raise SystemExit(1)

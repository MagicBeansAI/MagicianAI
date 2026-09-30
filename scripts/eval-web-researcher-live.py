#!/usr/bin/env python3
"""Public-API live gate for ordinary web-research tasks.

The live mode deliberately exercises the product path rather than calling a
model or search provider directly:

1. create a scoped task assigned either to ``web-researcher`` or to a parent
   agent that must delegate to it;
2. execute through the normal V3 runtime and wait for a user-readable output;
3. prove the execution lineage includes ``web-researcher``;
4. pair persisted ``tool.call.started``/``tool.call.finished`` events;
5. fetch bounded text from cited pages and have the governed evidence judge
   verify that the answer is supported by those opened pages;
6. score boundedness, required concepts, source domains, and reachability; and
7. join governed LLM-call telemetry for provider/model/cost diagnostics.

The default corpus is intentionally small. Answer-ready latency is measured and
reported, not used as a pass/fail deadline. While a case is running, press
Ctrl-C to stop it; the evaluator cancels and removes its disposable task before
writing the partial report. Source verification happens after the answer-ready
timestamp and therefore cannot make a fast agent look slow.

``--self-test`` and ``--dry-run`` are provider-free. They still write the same
JSON/HTML report shape used by the live lane and the ``/evals`` page.
"""

from __future__ import annotations

import argparse
import gzip
import json
import math
import os
import re
import statistics
import time
import unicodedata
import zlib
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from html import escape
from html.parser import HTMLParser
from pathlib import Path
from typing import Any, Callable, Iterable
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode, urlparse
from urllib.request import Request, build_opener


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = REPO_ROOT / "scripts/fixtures/web_researcher_live/cases.json"
DEFAULT_BASE_URL = "http://127.0.0.1:3002"
DEFAULT_OUTPUT_DIR = REPO_ROOT / "coverage/evals/web-researcher/live/latest"
WEB_RESEARCHER_AGENT_ID = "web-researcher"
# Keep live-eval spend bounded without changing production routing. Luna is the
# default, but callers may select any configured named profile for comparisons.
# The same profile is applied to the V3 task execution and the eval-only judge.
DEFAULT_EVAL_LLM_PROFILE = "gpt6luna-responses-toolsany"
LLM_PROFILE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$")
TERMINAL_STATUSES = {"completed", "failed", "cancelled", "canceled"}
# The agentic loop's own terminal, in both spellings the runtime emits.
AGENTIC_TERMINAL_EVENT_TYPES = {"agentic.execution.completed", "agentic.execution_completed"}
LIFECYCLE_TERMINAL_EVENT_TYPES = {
    "execution.completed",
    "execution.failed",
    "execution.cancelled",
    "execution.canceled",
}
PAUSED_STATUSES = {
    "waiting_for_user",
    "waiting_for_confirmation",
    "paused",
    "paused_by_user",
    "blocked",
}

TERM_NORMALIZATION_TRANSLATION = str.maketrans(
    {
        "\u00a0": " ",  # no-break space
        "\u00ad": "",  # soft hyphen
        "\u2010": "-",
        "\u2011": "-",  # non-breaking hyphen used by rich HTML synthesis
        "\u2012": "-",
        "\u2013": "-",
        "\u2014": "-",
        "\u2015": "-",
        "\u2212": "-",
        "\ufe58": "-",
        "\ufe63": "-",
        "\uff0d": "-",
    }
)
EXPECTED_WEB_RESEARCHER_TOOLS = frozenset(
    {
        "content_search",
        "content_read",
        "web_fetch",
        "working_set_search",
        "working_set_read",
        "research-working-sets",
        "deep-research-with-openai",
        "reddit-search",
        "hackernews-search",
        "github-search",
        "producthunt-search",
        "arxiv-search",
        "polymarket-search",
        "youtube-search",
        "catchup_merge",
        "browser",
    }
)
RETIRED_WEB_RESEARCHER_TOOLS = frozenset(
    {
        "content_search_batch",
        "content_read_batch",
        "websearch",
        "news-search-via-tavily",
        "semantic-websearch-via-exa",
        "websearch-via-openai",
        "find-details-by-username",
        "web-search-via-minimax",
        "whatsgoingon2",
        "vector",
        "htmltotext",
        "web-scraping-playbook",
        "eli5-explainer",
    }
)
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
MAX_OUTPUT_BYTES = 4 * 1024 * 1024
MAX_ANSWER_REPORT_CHARS = 6000
MAX_CITATION_EVIDENCE_BYTES = 4 * 1024 * 1024
MAX_CITATION_EVIDENCE_CHARS = 12_000
POLL_INTERVAL_SECONDS = 1.0
TELEMETRY_SETTLE_SECONDS = 30.0
ANALYTICS_BUSY_MAX_ATTEMPTS = 4
ANALYTICS_BUSY_BASE_BACKOFF_SECONDS = 0.25
URL_RE = re.compile(r"https?://[^\s<>'\"\])}]+", re.IGNORECASE)
MARKDOWN_URL_RE = re.compile(r"\[[^\]]+\]\((https?://[^\s)]+)\)", re.IGNORECASE)


class EvalFailure(RuntimeError):
    pass


class EvalHttpFailure(EvalFailure):
    def __init__(self, method: str, path: str, status: int, payload: Any):
        self.method = method
        self.path = path
        self.status = status
        self.payload = payload
        super().__init__(f"{method} {path} failed with HTTP {status}: {payload}")


class AnalyticsInfrastructureInconclusive(EvalFailure):
    pass


def eval_execution_request(
    llm_profile: str = DEFAULT_EVAL_LLM_PROFILE,
    delegate_to_agent: str | None = None,
) -> dict[str, Any]:
    """Execution-tree-local named route used by every live-eval task root.

    The broad lanes cover synthesis, reflection, correction and memory work;
    exact entries cover operations that intentionally do not inherit a broad
    lane or whose identity is important to the eval report.
    """
    endpoint = {"profile": llm_profile}
    request: dict[str, Any] = {
        "llm_routing_overrides": {
            "planning": endpoint,
            "evaluation": endpoint,
            "correction_extraction": endpoint,
            "memory_consolidation": endpoint,
            "operations": {
                "agentic_decision": endpoint,
                "evidence_precision_judge": endpoint,
                "durable_task_state_generate": endpoint,
                "durable_task_state_patch": endpoint,
                "durable_task_state_close_summary": endpoint,
            }
        }
    }
    if delegate_to_agent:
        request["delegate_to_agent"] = delegate_to_agent
    return request


@dataclass
class Gate:
    name: str
    passed: bool
    detail: str
    case_id: str = "suite"
    latency_ms: float = 0.0
    # A coverage gate measures how much of the request was delivered
    # (terms, domains, citation counts). On a terminal the runtime marked
    # `partial`, coverage decides full-vs-partial; it does not fail the case.
    # Every other gate is an assertion gate and fails the case as before.
    coverage: bool = False


@dataclass
class ToolCall:
    call_id: str
    tool_name: str
    success: bool
    duration_ms: float | None = None
    error: str | None = None


@dataclass
class CitationProbe:
    url: str
    status: int | None
    latency_ms: float
    error: str | None = None
    final_url: str | None = None
    title: str | None = None
    excerpt: str = ""


@dataclass
class CaseResult:
    case_id: str
    fixture_id: str
    repeat: int
    mode: str
    root_agent_id: str
    task_id: str = ""
    root_execution_id: str = ""
    execution_ids: list[str] = field(default_factory=list)
    execution_agents: list[str] = field(default_factory=list)
    root_status: str = "unknown"
    answer_ready_ms: float = 0.0
    answer_observed_at_ms: int = 0
    answer_chars: int = 0
    answer_words: int = 0
    answer_excerpt: str = ""
    citations: list[str] = field(default_factory=list)
    citation_probes: list[CitationProbe] = field(default_factory=list)
    judge_supported: bool | None = None
    judge_reason: str = ""
    judge_latency_ms: float = 0.0
    # Two-half grading. `completion_kind` and `open_items` come from the
    # task state the runtime propagated (full | partial); `declared_open` is
    # what the judge found the answer declaring about itself; `honesty` is
    # whether the declared-open items were genuinely unresolvable from the
    # agent's OWN opened evidence (honest | resolvable | unchecked).
    completion_kind: str | None = None
    open_items: list[str] = field(default_factory=list)
    declared_open: list[str] = field(default_factory=list)
    honesty: str | None = None
    honesty_reason: str = ""
    # `contradicted` | `absent` | None — the judge's classification of an
    # unsupported assertion. On a partial, only `contradicted` fails.
    judge_unsupported_kind: str | None = None
    verdict: str = "unscored"
    tool_calls: list[ToolCall] = field(default_factory=list)
    llm_calls: list[dict[str, Any]] = field(default_factory=list)
    terminal_lifecycle_events: list[dict[str, Any]] = field(default_factory=list)
    # Every journal row the eval read (not serialized into the report).
    settled_events: list[dict[str, Any]] = field(default_factory=list, repr=False)
    phase_timings_ms: dict[str, float] = field(default_factory=dict)
    output_id: str | None = None
    output_media_type: str | None = None
    error: str | None = None
    interrupted: bool = False
    infrastructure_inconclusive: list[str] = field(default_factory=list)


class _HtmlTextAndLinks(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.text: list[str] = []
        self.links: list[str] = []
        self.title: list[str] = []
        self._ignored_depth = 0
        self._in_title = False

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        folded = tag.casefold()
        if folded in {"script", "style", "template", "noscript"}:
            self._ignored_depth += 1
        if folded == "title":
            self._in_title = True
        if folded == "a":
            for key, value in attrs:
                if key.casefold() == "href" and value:
                    self.links.append(value)

    def handle_endtag(self, tag: str) -> None:
        folded = tag.casefold()
        if folded in {"script", "style", "template", "noscript"}:
            self._ignored_depth = max(0, self._ignored_depth - 1)
        if folded == "title":
            self._in_title = False

    def handle_data(self, data: str) -> None:
        if self._ignored_depth == 0 and data.strip():
            self.text.append(data.strip())
            if self._in_title:
                self.title.append(data.strip())


class Client:
    def __init__(self, base_url: str, timeout: float):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.opener = build_opener()

    def _url(self, path: str, query: dict[str, Any] | None) -> str:
        params = {key: value for key, value in (query or {}).items() if value is not None}
        if not params:
            return f"{self.base_url}{path}"
        separator = "&" if "?" in path else "?"
        return f"{self.base_url}{path}{separator}{urlencode(params)}"

    def request(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        *,
        accept: str = "application/json",
        max_bytes: int = MAX_RESPONSE_BYTES,
    ) -> tuple[int, bytes, float, str]:
        data = json.dumps(body).encode("utf-8") if body is not None else None
        request = Request(
            self._url(path, query),
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
        content_type = ""
        try:
            response = self.opener.open(request, timeout=self.timeout)
            status = response.status
            content_type = response.headers.get("Content-Type", "")
            raw = response.read(max_bytes + 1)
        except HTTPError as error:
            status = error.code
            content_type = error.headers.get("Content-Type", "") if error.headers else ""
            raw = error.read(max_bytes + 1)
        except (URLError, TimeoutError, OSError) as error:
            raise EvalFailure(
                f"runtime API unavailable during {method} {path} at {self.base_url}: {error}"
            ) from error
        latency_ms = (time.perf_counter() - started) * 1000
        if len(raw) > max_bytes:
            raise EvalFailure(f"response exceeded {max_bytes} bytes: {path}")
        return status, raw, latency_ms, content_type

    def json(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        expected: Iterable[int] = (200,),
    ) -> tuple[Any, float, int]:
        status, raw, latency, _ = self.request(method, path, body, query)
        try:
            payload = json.loads(raw) if raw else None
        except json.JSONDecodeError as error:
            raise EvalFailure(f"invalid JSON from {method} {path} (HTTP {status})") from error
        if status not in set(expected):
            raise EvalHttpFailure(method, path, status, payload)
        return payload, latency, status

    def ndjson(
        self, path: str, query: dict[str, Any] | None = None
    ) -> tuple[list[dict[str, Any]], float]:
        status, raw, latency, _ = self.request(
            "GET", path, query=query, accept="application/x-ndjson"
        )
        if status != 200:
            raise EvalFailure(f"GET {path} failed with HTTP {status}: {raw[:500]!r}")
        rows: list[dict[str, Any]] = []
        for line in raw.decode("utf-8", errors="replace").splitlines():
            if not line.strip():
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError as error:
                raise EvalFailure(f"invalid NDJSON row from {path}: {line[:200]}") from error
            if isinstance(value, dict):
                rows.append(value)
        return rows, latency


def nested_string(value: Any, keys: Iterable[str]) -> str | None:
    wanted = set(keys)
    stack = [value]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            for key, candidate in node.items():
                if key in wanted and isinstance(candidate, str) and candidate.strip():
                    return candidate.strip()
                if isinstance(candidate, (dict, list)):
                    stack.append(candidate)
        elif isinstance(node, list):
            stack.extend(candidate for candidate in node if isinstance(candidate, (dict, list)))
    return None


def task_id_from_response(payload: Any) -> str:
    task = payload.get("task") if isinstance(payload, dict) else None
    manifest = task.get("manifest") if isinstance(task, dict) else None
    task_id = manifest.get("task_id") if isinstance(manifest, dict) else None
    if not isinstance(task_id, str) or not task_id.strip():
        raise EvalFailure(f"task create response has no task id: {payload}")
    return task_id.strip()


def execution_id_from_response(payload: Any) -> str:
    execution = payload.get("execution") if isinstance(payload, dict) else None
    state = execution.get("state") if isinstance(execution, dict) else None
    execution_id = state.get("execution_id") if isinstance(state, dict) else None
    if not isinstance(execution_id, str) or not execution_id.strip():
        raise EvalFailure(f"task execute response has no execution id: {payload}")
    return execution_id.strip()


def load_cases(path: Path) -> list[dict[str, Any]]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvalFailure(f"cannot load web-research fixture {path}: {error}") from error
    if not isinstance(payload, dict) or payload.get("schema_version") != 1:
        raise EvalFailure(f"fixture must use schema_version 1: {path}")
    raw_cases = payload.get("cases")
    if not isinstance(raw_cases, list) or not raw_cases:
        raise EvalFailure(f"fixture has no cases: {path}")
    cases: list[dict[str, Any]] = []
    seen: set[str] = set()
    for raw in raw_cases:
        if not isinstance(raw, dict):
            raise EvalFailure("every fixture case must be an object")
        case_id = str(raw.get("id") or "").strip()
        mode = str(raw.get("mode") or "").strip()
        root_agent = str(raw.get("root_agent_id") or "").strip()
        query = str(raw.get("query") or "").strip()
        if not case_id or case_id in seen:
            raise EvalFailure(f"fixture case has missing or duplicate id: {case_id!r}")
        if mode not in {"direct", "delegated"}:
            raise EvalFailure(f"{case_id}: mode must be direct or delegated")
        if not root_agent or not query:
            raise EvalFailure(f"{case_id}: root_agent_id and query are required")
        if mode == "direct" and root_agent != WEB_RESEARCHER_AGENT_ID:
            raise EvalFailure(f"{case_id}: direct case must run web-researcher")
        if mode == "delegated" and WEB_RESEARCHER_AGENT_ID not in query.casefold():
            raise EvalFailure(f"{case_id}: delegated query must explicitly request web-researcher")
        for field_name in (
            "min_answer_chars",
            "max_answer_words",
            "min_citations",
            "min_distinct_domains",
            "min_resolvable_citations",
            "min_paired_tool_calls",
            "max_failed_tool_calls",
            "max_failed_llm_calls",
        ):
            value = raw.get(field_name)
            if not isinstance(value, int) or value < 0:
                raise EvalFailure(f"{case_id}: {field_name} must be a non-negative integer")
        for group_field in (
            "required_domain_groups",
            "required_term_groups",
            "required_tool_groups",
        ):
            groups = raw.get(group_field)
            if not isinstance(groups, list) or not groups:
                raise EvalFailure(f"{case_id}: {group_field} must be a non-empty array")
            if any(
                not isinstance(group, list)
                or not group
                or any(not isinstance(item, str) or not item.strip() for item in group)
                for group in groups
            ):
                raise EvalFailure(f"{case_id}: {group_field} contains an invalid group")
        seen.add(case_id)
        cases.append(raw)
    return cases


def declared_agent_tools(definition_yaml: str) -> set[str]:
    """Read only the top-level tools list without adding a YAML dependency."""
    tools: set[str] = set()
    in_tools = False
    for line in definition_yaml.splitlines():
        if line == "tools:":
            in_tools = True
            continue
        if not in_tools:
            continue
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if not line[0].isspace() and not line.startswith("-"):
            break
        if stripped.startswith("- "):
            value = stripped[2:].strip().strip("'\"")
            if value:
                tools.add(value)
    return tools


def normalize_event_type(event: dict[str, Any]) -> str:
    # AgentEvent envelopes carry a generic outer ``event_type=AgentEvent`` and
    # the useful AG-UI taxonomy under ``data.event.event_type``. Collect all
    # candidates so an outer wrapper cannot hide tool lifecycle evidence.
    candidates: list[str] = []
    stack: list[Any] = [event]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            for key, value in node.items():
                if key in {"event_type", "type"} and isinstance(value, str) and value.strip():
                    candidates.append(value.strip())
                if isinstance(value, (dict, list)):
                    stack.append(value)
        elif isinstance(node, list):
            stack.extend(value for value in node if isinstance(value, (dict, list)))
    raw = next(
        (value for value in candidates if "tool.call." in value.casefold()),
        next((value for value in candidates if value.casefold() != "agentevent"), ""),
    )
    dotted = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", ".", raw)
    return dotted.casefold().replace("_", ".").replace("-", ".")


def event_timestamp_ms(event: dict[str, Any]) -> int | None:
    """Read the canonical event time from disk or transport envelopes."""

    def parse(value: Any) -> int | None:
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            return int(value)
        if isinstance(value, str) and value.strip():
            try:
                parsed = datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
            except ValueError:
                return None
            return int(parsed.timestamp() * 1000)
        return None

    for node in (
        event,
        event.get("payload"),
        event.get("data"),
        (event.get("data") or {}).get("event")
        if isinstance(event.get("data"), dict)
        else None,
    ):
        if not isinstance(node, dict):
            continue
        for key in ("timestamp_ms", "timestamp", "finished_at", "ended_at"):
            parsed = parse(node.get(key))
            if parsed is not None:
                return parsed
        payload = node.get("payload")
        if isinstance(payload, dict):
            for key in ("timestamp_ms", "timestamp", "finished_at", "ended_at"):
                parsed = parse(payload.get(key))
                if parsed is not None:
                    return parsed
    return None


def terminal_lifecycle_events(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Project only lifecycle rows that can make a task/execution terminal."""
    projected: list[dict[str, Any]] = []
    direct_terminal_types = {
        "agentic.execution.completed",
        # The V3 journal spells the agentic terminal with an underscore
        # (`ArtifactV2EventType::AgenticExecutionCompleted`); the dot form is
        # the realtime taxonomy. Both are the same terminal.
        "agentic.execution_completed",
        "execution.completed",
        "execution.failed",
        "execution.cancelled",
        "execution.canceled",
    }
    for event in events:
        event_type = normalize_event_type(event)
        status = nested_string(
            event,
            ("new_status", "execution_status", "task_status", "outcome", "status"),
        )
        status_folded = (status or "").casefold()
        if event_type not in direct_terminal_types and not (
            event_type == "execution.status.changed"
            and status_folded in TERMINAL_STATUSES
        ):
            continue
        execution_id = str(event.get("execution_id") or "").strip()
        if not execution_id:
            execution_id = nested_string(event, ("execution_id",)) or ""
        projected.append(
            {
                "event_type": event_type,
                "execution_id": execution_id,
                "status": status_folded or None,
                "timestamp_ms": event_timestamp_ms(event),
            }
        )
    return projected


def tool_lineage_metadata(event: dict[str, Any]) -> dict[str, Any] | None:
    """Find the canonical lineage payload inside transport wrappers."""
    stack: list[Any] = [event]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            if (
                isinstance(node.get("stage"), str)
                and isinstance(node.get("tool_execution_id"), str)
                and isinstance(node.get("tool_name"), str)
            ):
                return node
            stack.extend(
                candidate
                for candidate in node.values()
                if isinstance(candidate, (dict, list))
            )
        elif isinstance(node, list):
            stack.extend(
                candidate for candidate in node if isinstance(candidate, (dict, list))
            )
    return None


def tool_calls_from_events(events: list[dict[str, Any]]) -> tuple[list[ToolCall], int]:
    started: dict[str, str] = {}
    started_at_ms: dict[str, float] = {}
    finished: dict[str, ToolCall] = {}
    for event in events:
        kind = normalize_event_type(event)
        lineage = tool_lineage_metadata(event) if "llm.tool.lineage" in kind else None
        if lineage is not None:
            stage = str(lineage.get("stage") or "").casefold()
            call_id = str(lineage.get("tool_execution_id") or "").strip()
            tool_name = str(lineage.get("tool_name") or "unknown").strip() or "unknown"
            occurred_at_ms = lineage.get("occurred_at_ms")
            if stage == "execution_started" and call_id:
                started[call_id] = tool_name
                if isinstance(occurred_at_ms, (int, float)) and not isinstance(
                    occurred_at_ms, bool
                ):
                    started_at_ms[call_id] = float(occurred_at_ms)
            elif stage == "execution_finished" and call_id:
                reported_success = lineage.get("tool_reported_success")
                outcome = str(lineage.get("outcome") or "").casefold()
                success = reported_success is not False and outcome not in {
                    "failed",
                    "error",
                    "cancelled",
                    "canceled",
                }
                duration = None
                if (
                    isinstance(occurred_at_ms, (int, float))
                    and not isinstance(occurred_at_ms, bool)
                    and call_id in started_at_ms
                ):
                    duration = max(0.0, float(occurred_at_ms) - started_at_ms[call_id])
                finished[call_id] = ToolCall(
                    call_id=call_id,
                    tool_name=tool_name or started.get(call_id, "unknown"),
                    success=success,
                    duration_ms=duration,
                    error=(
                        str(lineage.get("failure_code") or "tool execution failed")
                        if not success
                        else None
                    ),
                )
        elif "tool.call.started" in kind:
            call_id = nested_string(event, ("call_id",)) or ""
            if call_id:
                started[call_id] = nested_string(event, ("tool_name",)) or "unknown"
        elif "tool.call.finished" in kind or "tool.call.failed" in kind:
            call_id = nested_string(event, ("call_id",)) or ""
            if not call_id:
                continue
            success_value = nested_value(event, "success")
            success = success_value is not False and "failed" not in kind
            duration = nested_number(event, "duration_ms")
            finished[call_id] = ToolCall(
                call_id=call_id,
                tool_name=nested_string(event, ("tool_name",)) or started.get(call_id, "unknown"),
                success=success,
                duration_ms=duration,
                error=nested_string(event, ("error",)) if not success else None,
            )
    paired = len(set(started).intersection(finished))
    return list(finished.values()), paired


def tool_calls_from_fact_rows(rows: list[dict[str, Any]]) -> tuple[list[ToolCall], int]:
    """Project canonical ``llm_tool_calls`` lifecycle facts into eval calls."""
    started: dict[str, tuple[str, float | None]] = {}
    finished: dict[str, tuple[float, ToolCall]] = {}
    for row in rows:
        call_id = str(row.get("tool_execution_id") or "").strip()
        if not call_id:
            continue
        stage = str(row.get("tool_lineage_stage") or "").casefold()
        tool_name = str(row.get("tool_name") or "unknown").strip() or "unknown"
        observed_at = row.get("observed_at_ms")
        observed_at_ms = (
            float(observed_at)
            if isinstance(observed_at, (int, float)) and not isinstance(observed_at, bool)
            else None
        )
        if stage == "execution_started":
            previous = started.get(call_id)
            if previous is None or (
                observed_at_ms is not None
                and (previous[1] is None or observed_at_ms < previous[1])
            ):
                started[call_id] = (tool_name, observed_at_ms)
            continue
        if stage != "execution_finished":
            continue
        reported_success = row.get("tool_reported_success")
        outcome = str(row.get("tool_outcome") or "").casefold()
        success = reported_success is not False and outcome not in {
            "failed",
            "denied",
            "cancelled",
            "canceled",
            "timed_out",
            "abandoned",
        }
        started_name, started_ms = started.get(call_id, (tool_name, None))
        duration = (
            max(0.0, observed_at_ms - started_ms)
            if observed_at_ms is not None and started_ms is not None
            else None
        )
        sort_key = observed_at_ms if observed_at_ms is not None else float("inf")
        finished[call_id] = (
            sort_key,
            ToolCall(
                call_id=call_id,
                tool_name=tool_name or started_name,
                success=success,
                duration_ms=duration,
                error=(
                    str(row.get("tool_failure_code") or "tool execution failed")
                    if not success
                    else None
                ),
            ),
        )
    paired = len(set(started).intersection(finished))
    return [entry[1] for entry in sorted(finished.values(), key=lambda entry: entry[0])], paired


def configure_ca_bundle(candidate: str | None = None) -> bool:
    """Supply verified roots for standalone macOS Python installations."""
    if os.environ.get("SSL_CERT_FILE", "").strip():
        return False
    if candidate is None:
        try:
            import certifi
        except ImportError:
            return False
        candidate = certifi.where()
    if not candidate or not Path(candidate).is_file():
        return False
    os.environ["SSL_CERT_FILE"] = candidate
    return True


def nested_value(value: Any, key: str) -> Any:
    stack = [value]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            if key in node:
                return node[key]
            stack.extend(candidate for candidate in node.values() if isinstance(candidate, (dict, list)))
        elif isinstance(node, list):
            stack.extend(candidate for candidate in node if isinstance(candidate, (dict, list)))
    return None


def nested_number(value: Any, key: str) -> float | None:
    candidate = nested_value(value, key)
    if isinstance(candidate, (int, float)) and not isinstance(candidate, bool):
        return float(candidate)
    return None


def phase_timings_from_events(events: list[dict[str, Any]]) -> dict[str, float]:
    """Collect bounded runtime phase diagnostics from this execution tree."""
    timings: dict[str, float] = {}
    for event in events:
        stack: list[Any] = [event]
        while stack:
            node = stack.pop()
            if isinstance(node, dict):
                phase_values = node.get("phase_timing_ms")
                if isinstance(phase_values, dict):
                    for raw_name, raw_value in phase_values.items():
                        if (
                            isinstance(raw_name, str)
                            and isinstance(raw_value, (int, float))
                            and not isinstance(raw_value, bool)
                        ):
                            name = raw_name.strip()
                            if name:
                                timings[name] = max(
                                    timings.get(name, 0.0), float(raw_value)
                                )
                if node.get("mode") == "delegation_checkpoint_delta":
                    resume_ms = node.get("resume_ms")
                    if isinstance(resume_ms, (int, float)) and not isinstance(
                        resume_ms, bool
                    ):
                        timings["delegation_checkpoint_resume"] = max(
                            timings.get("delegation_checkpoint_resume", 0.0),
                            float(resume_ms),
                        )
                stack.extend(
                    candidate
                    for candidate in node.values()
                    if isinstance(candidate, (dict, list))
                )
            elif isinstance(node, list):
                stack.extend(
                    candidate
                    for candidate in node
                    if isinstance(candidate, (dict, list))
                )
    return timings


def text_and_citations(raw: bytes, media_type: str) -> tuple[str, list[str]]:
    decoded = raw.decode("utf-8", errors="replace")
    urls: list[str] = []
    if "html" in media_type.casefold() or "<html" in decoded[:1000].casefold():
        parser = _HtmlTextAndLinks()
        parser.feed(decoded)
        text = " ".join(parser.text)
        urls.extend(parser.links)
    else:
        text = decoded
    urls.extend(MARKDOWN_URL_RE.findall(decoded))
    urls.extend(URL_RE.findall(decoded))
    cleaned_text = re.sub(r"\s+", " ", text).strip()
    normalized_urls: list[str] = []
    for url in urls:
        candidate = url.strip().rstrip(".,;:!?")
        parsed = urlparse(candidate)
        if parsed.scheme in {"http", "https"} and parsed.hostname and candidate not in normalized_urls:
            normalized_urls.append(candidate)
    return cleaned_text, normalized_urls


def hostname(url: str) -> str:
    return (urlparse(url).hostname or "").casefold().rstrip(".")


def host_matches(host: str, expected: str) -> bool:
    expected = expected.casefold().strip().rstrip(".")
    return host == expected or host.endswith("." + expected)


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, math.ceil(quantile * len(ordered)) - 1))
    return ordered[index]


def value_is_false(value: Any) -> bool:
    return value is False or value == 0 or (
        isinstance(value, str) and value.strip().casefold() in {"false", "0", "failed"}
    )


def decode_http_content(
    raw: bytes,
    content_encoding: str,
    limit: int = MAX_CITATION_EVIDENCE_BYTES,
    input_truncated: bool = False,
) -> tuple[bytes, bool]:
    """Decode gzip/deflate transfer encodings without unbounded inflation."""

    decoded = raw
    truncated = False
    encodings = [
        item.strip().casefold()
        for item in content_encoding.split(",")
        if item.strip() and item.strip().casefold() != "identity"
    ]
    for encoding in reversed(encodings):
        if encoding == "gzip":
            wbits_candidates = (zlib.MAX_WBITS | 16,)
        elif encoding == "deflate":
            # RFC 7230 uses the zlib wrapper. A few servers still emit raw
            # DEFLATE, so accept it only as a compatibility fallback.
            wbits_candidates = (zlib.MAX_WBITS, -zlib.MAX_WBITS)
        else:
            raise ValueError(f"unsupported Content-Encoding: {encoding}")

        last_error: zlib.error | None = None
        for wbits in wbits_candidates:
            try:
                inflater = zlib.decompressobj(wbits)
                candidate = inflater.decompress(decoded, limit + 1)
                if len(candidate) <= limit and inflater.eof:
                    candidate += inflater.flush(limit + 1 - len(candidate))
                layer_truncated = len(candidate) > limit or bool(inflater.unconsumed_tail)
                if not inflater.eof and not layer_truncated and not input_truncated:
                    last_error = zlib.error(f"incomplete {encoding} stream")
                    continue
                decoded = candidate[:limit]
                truncated = truncated or layer_truncated or (input_truncated and not inflater.eof)
                break
            except zlib.error as error:
                last_error = error
        else:
            assert last_error is not None
            raise last_error

    if len(decoded) > limit:
        return decoded[:limit], True
    return decoded, truncated


def verify_citation(url: str, timeout: float = 15.0) -> CitationProbe:
    request = Request(
        url,
        method="GET",
        headers={
            "Accept": "text/html,application/xhtml+xml,application/json;q=0.8,*/*;q=0.1",
            "Accept-Encoding": "gzip, deflate",
            "User-Agent": "WebResearchLiveEval/1.0",
        },
    )
    started = time.perf_counter()
    try:
        response = build_opener().open(request, timeout=timeout)
        status = response.status
        raw = response.read(MAX_CITATION_EVIDENCE_BYTES + 1)
        final_url = response.geturl()
        content_type = response.headers.get_content_type().casefold()
        charset = response.headers.get_content_charset() or "utf-8"
        raw_truncated = len(raw) > MAX_CITATION_EVIDENCE_BYTES
        content_encoding = response.headers.get("Content-Encoding", "")
        if not isinstance(content_encoding, str):
            content_encoding = ""
        evidence, decoded_truncated = decode_http_content(
            raw[:MAX_CITATION_EVIDENCE_BYTES],
            content_encoding,
            input_truncated=raw_truncated,
        )
        truncated = raw_truncated or decoded_truncated
        decoded = evidence.decode(charset, errors="replace")
        title: str | None = None
        excerpt = ""
        if content_type in {"text/html", "application/xhtml+xml"}:
            parser = _HtmlTextAndLinks()
            parser.feed(decoded)
            excerpt = " ".join(parser.text)
            title = " ".join(parser.title).strip() or None
        elif content_type.startswith("text/") or content_type in {
            "application/json",
            "application/ld+json",
        }:
            excerpt = decoded
        excerpt = re.sub(r"\s+", " ", excerpt).strip()
        excerpt = excerpt[:MAX_CITATION_EVIDENCE_CHARS]
        return CitationProbe(
            url=url,
            status=status,
            latency_ms=(time.perf_counter() - started) * 1000,
            final_url=final_url,
            title=title,
            excerpt=excerpt,
            error=(
                f"response exceeded {MAX_CITATION_EVIDENCE_BYTES} bytes; "
                "judge evidence uses the retained bounded prefix"
                if truncated
                else None
            ),
        )
    except HTTPError as error:
        return CitationProbe(
            url=url,
            status=error.code,
            latency_ms=(time.perf_counter() - started) * 1000,
            error=f"HTTP {error.code}",
            final_url=error.geturl(),
        )
    except (URLError, TimeoutError, OSError, ValueError, zlib.error, gzip.BadGzipFile) as error:
        return CitationProbe(
            url=url,
            status=None,
            latency_ms=(time.perf_counter() - started) * 1000,
            error=str(error),
        )


def judge_answer(
    client: Client,
    question: str,
    answer: str,
    probes: list[CitationProbe],
    llm_profile: str = DEFAULT_EVAL_LLM_PROFILE,
    own_sources: list[dict[str, Any]] | None = None,
    observed_actions: list[str] | None = None,
) -> tuple[bool, str, float, list[str]]:
    own_sources = own_sources or []
    sources = [
        {
            "url": probe.url,
            "final_url": probe.final_url,
            "title": probe.title,
            "excerpt": probe.excerpt,
        }
        for probe in probes
        if probe.status is not None
        and 200 <= probe.status < 400
        and probe.excerpt.strip()
    ]
    # The pages the agent itself opened are sources too. A fresh probe can
    # land on a redirect page or a JavaScript shell the agent never saw, and
    # grading only against that scores the agent for the eval's fetch luck.
    # Same URL, two readings: a fresh probe can land on a redirect stub or a
    # JavaScript shell while the agent's own read has the article. Keep the
    # fuller reading of each URL; a run must not be graded on the eval's
    # fetch luck when its own evidence is on file.
    by_url = {str(source["url"]): source for source in sources}
    for source in own_sources:
        url = str(source["url"])
        existing = by_url.get(url)
        if existing is None:
            if len(by_url) >= MAX_OWN_EVIDENCE_SOURCES:
                continue
            by_url[url] = source
        elif len(str(source.get("excerpt") or "")) > len(str(existing.get("excerpt") or "")):
            by_url[url] = {**existing, "excerpt": source["excerpt"], "title": source.get("title") or existing.get("title")}
    sources = list(by_url.values())
    if not sources:
        return False, "no readable text was captured from cited or opened pages", 0.0, []
    return judge_sources(
        client, question, answer, sources, llm_profile, "web_research_answer", observed_actions
    )


def tool_inventory_lines(tool_calls: list[ToolCall]) -> list[str]:
    """`name xN (M failed)` per tool, in first-seen order — what the run did."""
    counts: dict[str, list[int]] = {}
    for call in tool_calls:
        entry = counts.setdefault(call.tool_name, [0, 0])
        entry[0] += 1
        if not call.success:
            entry[1] += 1
    return [
        f"{name} x{total}" + (f" ({failed} failed)" if failed else "")
        for name, (total, failed) in counts.items()
    ]


def judge_sources(
    client: Client,
    question: str,
    answer: str,
    sources: list[dict[str, Any]],
    llm_profile: str,
    evidence_kind: str,
    observed_actions: list[str] | None = None,
) -> tuple[bool, str, float, list[str]]:
    payload, latency, _ = client.json(
        "POST",
        "/api/magician/v2/evals/web-researcher/judge",
        {
            "question": question,
            "answer": answer,
            "sources": sources,
            "llm_profile": llm_profile,
            "evidence_kind": evidence_kind,
            "observed_actions": observed_actions or [],
        },
    )
    if not isinstance(payload, dict) or not isinstance(payload.get("supported"), bool):
        raise EvalFailure(f"invalid web-research judge response: {payload}")
    declared = payload.get("declared_open")
    declared_open = (
        [str(item) for item in declared if str(item).strip()] if isinstance(declared, list) else []
    )
    kind = payload.get("unsupported_kind")
    LAST_JUDGE_UNSUPPORTED_KIND[0] = kind if isinstance(kind, str) else None
    return bool(payload["supported"]), str(payload.get("reason") or ""), latency, declared_open


# The judge's last `unsupported_kind`, read back by the caller that scores the
# answer. A cell rather than a wider return type so the honesty call, which
# shares `judge_sources`, keeps its signature.
LAST_JUDGE_UNSUPPORTED_KIND: list[str | None] = [None]


# The judge endpoint's own ceilings (magician-api evals_api.rs).
MAX_OWN_EVIDENCE_SOURCES = 8
MAX_OWN_EVIDENCE_SOURCE_CHARS = 40_000


def opened_evidence_values(value: Any) -> list[dict[str, Any]]:
    """Every claim-eligible opened-page envelope in a tool result, by shape."""
    found: list[dict[str, Any]] = []

    def visit(node: Any) -> None:
        if isinstance(node, dict):
            if (
                node.get("claim_eligible") is True
                and node.get("evidence_role") == "opened_page"
                and node.get("fetch_status") in {"complete", "partial"}
            ):
                found.append(node)
                return
            for child in node.values():
                visit(child)
        elif isinstance(node, list):
            for child in node:
                visit(child)

    if isinstance(value, dict) and isinstance(value.get("evidence"), list):
        for item in value["evidence"]:
            visit(item)
    else:
        visit(value)
    return found


def own_opened_evidence(
    client: Client,
    task_id: str,
    execution_ids: list[str],
    started_at_ms: int,
) -> list[dict[str, Any]]:
    """The pages the agent itself opened during the run, as judge sources.

    Every tool result the run persisted is announced by an `artifact.created`
    event carrying its `execution_download_url`; those files are admitted by
    the same envelope shape the runtime's terminal grounding gate uses — never
    by tool name. A declared-open item is graded against what the agent
    actually had, not against what a fresh fetch would show.
    """
    events, _ = fetch_events(client, task_id, execution_ids, started_at_ms)
    urls: list[str] = []
    for event in events:
        if normalize_event_type(event) != "artifact.created":
            continue
        payload = event.get("payload") if isinstance(event.get("payload"), dict) else {}
        url = payload.get("execution_download_url")
        if isinstance(url, str) and url.endswith(".json"):
            urls.append(url)
    sources: dict[str, dict[str, Any]] = {}
    budget = MAX_OWN_EVIDENCE_SOURCE_CHARS
    for url_path in dict.fromkeys(urls):
        try:
            status, raw, _, _ = client.request(
                "GET", url_path, accept="*/*", max_bytes=MAX_OUTPUT_BYTES
            )
            if status != 200:
                continue
            value = json.loads(raw)
        except Exception:
            continue
        for opened in opened_evidence_values(value):
            document = opened.get("document") if isinstance(opened.get("document"), dict) else {}
            url = str(
                document.get("canonical_url")
                or opened.get("canonical_url")
                or opened.get("url")
                or opened.get("final_url")
                or ""
            ).strip()
            text = str(
                document.get("text") or opened.get("excerpt") or opened.get("text") or ""
            ).strip()
            if not url or not text:
                continue
            excerpt = text[: min(len(text), 6_000, max(budget, 0))]
            if not excerpt:
                continue
            existing = sources.get(url)
            if existing is not None and len(existing["excerpt"]) >= len(excerpt):
                continue
            budget -= len(excerpt) - (len(existing["excerpt"]) if existing else 0)
            sources[url] = {
                "url": url,
                "final_url": url,
                "title": str(document.get("title") or opened.get("title") or "") or None,
                "excerpt": excerpt,
            }
            if len(sources) >= MAX_OWN_EVIDENCE_SOURCES or budget <= 0:
                break
        if len(sources) >= MAX_OWN_EVIDENCE_SOURCES or budget <= 0:
            break
    return list(sources.values())


def judge_declared_open_honesty(
    client: Client,
    question: str,
    open_items: list[str],
    own_sources: list[dict[str, Any]],
    llm_profile: str,
) -> tuple[str, str]:
    """honest | resolvable | unchecked — were the open items genuinely open?"""
    if not open_items:
        return "honest", "nothing declared open"
    if not own_sources:
        return "unchecked", "the run left no claim-eligible opened evidence to grade against"
    supported, reason, _, _ = judge_sources(
        client,
        question,
        "\n".join(f"- {item}" for item in open_items),
        own_sources,
        llm_profile,
        "declared_open_items",
    )
    return ("honest" if supported else "resolvable"), reason


def task_state(payload: Any) -> dict[str, Any]:
    task = payload.get("task") if isinstance(payload, dict) else None
    state = task.get("state") if isinstance(task, dict) else None
    return state if isinstance(state, dict) else {}


def list_executions(client: Client, task_id: str) -> tuple[list[dict[str, Any]], float]:
    payload, latency, _ = client.json(
        "GET", f"/api/magician/v3/tasks/{quote(task_id)}/executions"
    )
    rows = payload.get("executions") if isinstance(payload, dict) else None
    if not isinstance(rows, list):
        raise EvalFailure(f"execution listing is invalid: {payload}")
    return [row for row in rows if isinstance(row, dict)], latency


def wait_for_answer(
    client: Client,
    task_id: str,
    root_execution_id: str,
) -> tuple[list[dict[str, Any]], dict[str, Any], float]:
    started = time.perf_counter()
    next_notice = 15.0
    last_status = "unknown"
    last_task_state: dict[str, Any] = {}
    while True:
        elapsed = time.perf_counter() - started
        executions, _ = list_executions(client, task_id)
        root = next(
            (row for row in executions if str(row.get("execution_id") or "") == root_execution_id),
            None,
        )
        if root is not None:
            last_status = str(root.get("status") or "unknown").casefold()
            if last_status in {"failed", "cancelled", "canceled"}:
                raise EvalFailure(f"root execution became terminal with status={last_status}")
            if last_status in PAUSED_STATUSES:
                raise EvalFailure(
                    "root execution paused with "
                    f"status={last_status}; unattended public-web evals cannot resume it"
                )
        payload, _, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}")
        last_task_state = task_state(payload)
        pending = last_task_state.get("synthesis_pending_executions")
        primary = last_task_state.get("primary_user_output_id")
        synthesis_clear = pending in (None, [])
        if last_status == "completed" and isinstance(primary, str) and primary and synthesis_clear:
            return executions, last_task_state, elapsed * 1000
        if elapsed >= next_notice:
            agents = sorted({str(row.get("agent_id") or "") for row in executions})
            print(
                f"  waiting {elapsed:.0f}s: root={last_status} agents={agents} "
                f"synthesis_pending={pending}",
                flush=True,
            )
            next_notice += 15.0
        time.sleep(POLL_INTERVAL_SECONDS)


def fetch_primary_output(
    client: Client, task_id: str, expected_output_id: str
) -> tuple[str, str, bytes, float]:
    payload, latency, _ = client.json(
        "GET", f"/api/magician/v3/tasks/{quote(task_id)}/outputs"
    )
    record = payload.get("outputs") if isinstance(payload, dict) else None
    outputs = record.get("outputs") if isinstance(record, dict) else None
    if not isinstance(outputs, list):
        raise EvalFailure(f"task outputs response is invalid: {payload}")
    chosen = next(
        (
            row
            for row in outputs
            if isinstance(row, dict) and str(row.get("output_id") or "") == expected_output_id
        ),
        None,
    )
    if chosen is None:
        raise EvalFailure(f"primary user output {expected_output_id!r} is absent from outputs")
    relative_path = str(chosen.get("relative_path") or "").strip()
    if not relative_path:
        raise EvalFailure("primary user output has no relative_path")
    status, raw, download_latency, content_type = client.request(
        "GET",
        f"/api/magician/v3/tasks/{quote(task_id)}/outputs/{quote(relative_path, safe='/')}",
        accept="*/*",
        max_bytes=MAX_OUTPUT_BYTES,
    )
    if status != 200:
        raise EvalFailure(f"primary output download failed with HTTP {status}")
    media_type = str(chosen.get("media_type") or content_type or "application/octet-stream")
    return expected_output_id, media_type, raw, latency + download_latency


def fetch_events(
    client: Client,
    task_id: str,
    execution_ids: list[str],
    started_at_ms: int,
) -> tuple[list[dict[str, Any]], float]:
    """Read only the per-execution journals owned by this eval task.

    Passing only ``task_id`` selects the cross-scope backfill path. That path is
    useful for workspace timelines, but it is too broad for a cost/quality eval:
    concurrent chat or Tutor events can otherwise be attributed to this case.
    An explicit task+execution pair selects the canonical single-journal path.
    """
    events: list[dict[str, Any]] = []
    total_latency = 0.0
    for execution_id in dict.fromkeys(value for value in execution_ids if value):
        rows, latency = client.ndjson(
            "/api/magician/v3/events",
            {
                "task_id": task_id,
                "execution_id": execution_id,
                "since": started_at_ms,
                "limit": 4000,
                "backfill_only": "true",
            },
        )
        events.extend(rows)
        total_latency += latency
    return events, total_latency


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
        "input_tokens, output_tokens, reasoning_tokens, cache_read_tokens, "
        "cache_creation_tokens, queue_wait_ms, local_prep_ms, "
        "provider_execution_ms, latency_ms, cost_usd, "
        "task_id, root_execution_id, execution_id, iteration_id, "
        "prompt_projection_mode, trace_id FROM llm_calls "
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
    payload, latency, _ = analytics_json_with_busy_retry(
        client,
        "/api/magician/v2/analytics/llm/facts/query",
        {
            "sql": sql,
            "from_ms": started_window_ms,
            "to_ms": ended_window_ms,
            "limit": 1000,
        },
    )
    data = payload.get("data") if isinstance(payload, dict) else None
    rows = data.get("rows") if isinstance(data, dict) else None
    if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
        raise EvalFailure(f"LLM analytics response is invalid: {payload}")
    calls = [{str(name): value for name, value in row.items()} for row in rows]
    return calls, latency


def query_tool_calls(
    client: Client,
    task_id: str,
    execution_ids: list[str],
    started_at_ms: int,
    ended_at_ms: int,
) -> tuple[list[ToolCall], int, float]:
    """Read sanitized canonical tool lifecycle facts for one eval task."""
    safe_task = re.sub(r"[^A-Za-z0-9_.:-]", "", task_id)
    safe_execs = [re.sub(r"[^A-Za-z0-9_.:-]", "", value) for value in execution_ids]
    identities = [f"task_id = '{safe_task}'"]
    identities.extend(f"execution_id = '{value}'" for value in safe_execs if value)
    sql = (
        "SELECT observed_at_ms, tool_execution_id, tool_name, tool_lineage_stage, "
        "tool_outcome, tool_failure_code, tool_reported_success "
        "FROM llm_tool_calls "
        f"WHERE ({' OR '.join(identities)}) "
        "ORDER BY observed_at_ms, record_revision LIMIT 1000"
    )
    payload, latency, _ = analytics_json_with_busy_retry(
        client,
        "/api/magician/v2/analytics/llm/facts/query",
        {
            "sql": sql,
            "from_ms": started_at_ms - 2000,
            "to_ms": ended_at_ms + 60000,
            "limit": 1000,
        },
    )
    data = payload.get("data") if isinstance(payload, dict) else None
    rows = data.get("rows") if isinstance(data, dict) else None
    if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
        raise EvalFailure(f"tool analytics response is invalid: {payload}")
    calls, paired = tool_calls_from_fact_rows(rows)
    return calls, paired, latency


def analytics_failure_is_busy(error: Exception) -> bool:
    if isinstance(error, EvalHttpFailure):
        payload = error.payload if isinstance(error.payload, dict) else {}
        if payload.get("code") == "analytics_busy" or payload.get("retryable") is True:
            return True
        # This helper is used only by governed analytics reads. A 503 is an
        # admission/capacity result, not malformed SQL or auth.
        if error.status == 503:
            return True
    normalized = str(error).casefold()
    return any(
        marker in normalized
        for marker in (
            "duckdb is busy",
            "analytics duckdb guard",
            "query guard",
            "database is locked",
            "conflicting lock",
        )
    )


def analytics_json_with_busy_retry(
    client: Client,
    path: str,
    body: dict[str, Any],
) -> tuple[Any, float, int]:
    """Retry only transient analytics lock contention, never bad SQL/auth."""
    for attempt in range(1, ANALYTICS_BUSY_MAX_ATTEMPTS + 1):
        try:
            return client.json("POST", path, body)
        except EvalFailure as error:
            if not analytics_failure_is_busy(error):
                raise
            if attempt >= ANALYTICS_BUSY_MAX_ATTEMPTS:
                raise AnalyticsInfrastructureInconclusive(
                    f"analytics remained busy after {attempt} attempts: {error}"
                ) from error
            time.sleep(
                ANALYTICS_BUSY_BASE_BACKOFF_SECONDS * (2 ** (attempt - 1))
            )
    raise AssertionError("analytics retry loop exhausted without returning or raising")


def wait_for_llm_calls(
    client: Client,
    task_id: str,
    execution_ids: list[str],
    started_at_ms: int,
    ended_at_ms: int,
    required_operations: set[str] | None = None,
) -> tuple[list[dict[str, Any]], float]:
    required = required_operations or set()
    deadline = time.monotonic() + TELEMETRY_SETTLE_SECONDS
    total_latency = 0.0
    calls: list[dict[str, Any]] = []
    while time.monotonic() < deadline:
        calls, latency = query_llm_calls(
            client, task_id, execution_ids, started_at_ms, ended_at_ms
        )
        total_latency += latency
        observed_operations = {
            str(call.get("operation") or "") for call in calls
        }
        if calls and required.issubset(observed_operations):
            break
        time.sleep(1.0)
    return calls, total_latency


def wait_for_tool_calls(
    client: Client,
    task_id: str,
    execution_ids: list[str],
    started_at_ms: int,
    ended_at_ms: int,
) -> tuple[list[ToolCall], int, float]:
    deadline = time.monotonic() + TELEMETRY_SETTLE_SECONDS
    total_latency = 0.0
    calls: list[ToolCall] = []
    paired = 0
    while time.monotonic() < deadline:
        calls, paired, latency = query_tool_calls(
            client, task_id, execution_ids, started_at_ms, ended_at_ms
        )
        total_latency += latency
        if calls:
            break
        time.sleep(1.0)
    return calls, paired, total_latency


def cancel_active_execution(client: Client, root_execution_id: str) -> tuple[bool, str, float]:
    """Stop a still-running execution WITHOUT deleting its task.

    Retention keeps the run's decision events for inspection; it must not also
    leave the run burning tokens. Operator stop is the case that makes the
    distinction matter: the task is still `executing` when the eval unwinds.
    """
    status, raw, latency, _ = client.request(
        "POST",
        f"/api/magician/v3/executions/{quote(root_execution_id)}/cancel",
        body={},
    )
    detail = f"cancel HTTP {status}"
    return status in (200, 202, 204, 409), detail, latency


def cleanup_task(
    client: Client, task_id: str, root_execution_id: str | None = None
) -> tuple[bool, str, float]:
    status, raw, latency, _ = client.request(
        "DELETE",
        f"/api/magician/v3/tasks/{quote(task_id)}",
        query={"remove_files": "true"},
    )
    try:
        payload = json.loads(raw) if raw else None
    except json.JSONDecodeError:
        payload = raw.decode("utf-8", errors="replace")[:500]
    if (
        status == 400
        and isinstance(payload, dict)
        and str(payload.get("error") or "").startswith("task_active:")
    ):
        if root_execution_id:
            cancel_status, _, cancel_latency, _ = client.request(
                "POST",
                f"/api/magician/v3/executions/{quote(root_execution_id)}/cancel",
                body={},
            )
        else:
            cancel_status, _, cancel_latency, _ = client.request(
                "PUT",
                f"/api/magician/v3/tasks/{quote(task_id)}/status",
                body={"status": "cancelled"},
            )
        latency += cancel_latency
        if cancel_status in {200, 202, 409}:
            status, raw, retry_latency, _ = client.request(
                "DELETE",
                f"/api/magician/v3/tasks/{quote(task_id)}",
                query={"remove_files": "true"},
            )
            latency += retry_latency
            try:
                payload = json.loads(raw) if raw else None
            except json.JSONDecodeError:
                payload = raw.decode("utf-8", errors="replace")[:500]
    # APFS may report ENOTEMPTY for a just-fenced directory while a final
    # handle/rename settles. Product deletion retries too; retain a bounded
    # harness retry so cleanup transport timing cannot invalidate a factual
    # evaluation result.
    for retry_index in range(3):
        error_text = str(payload.get("error") or "") if isinstance(payload, dict) else str(payload)
        if status < 500 or "directory not empty" not in error_text.lower():
            break
        time.sleep(0.025 * (2**retry_index))
        status, raw, retry_latency, _ = client.request(
            "DELETE",
            f"/api/magician/v3/tasks/{quote(task_id)}",
            query={"remove_files": "true"},
        )
        latency += retry_latency
        try:
            payload = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            payload = raw.decode("utf-8", errors="replace")[:500]
    return status == 200, f"HTTP {status} payload={payload}", latency


def capture_partial_case_observability(
    client: Client,
    result: CaseResult,
    gates: list[Gate],
    started_at_ms: int,
) -> None:
    """Capture durable evidence before interrupted-task cleanup removes it."""
    if not result.task_id:
        return
    case_id = result.case_id
    execution_ids = [result.root_execution_id] if result.root_execution_id else []
    try:
        executions, latency = list_executions(client, result.task_id)
        observed_ids = [
            str(row.get("execution_id"))
            for row in executions
            if isinstance(row.get("execution_id"), str)
        ]
        if observed_ids:
            execution_ids = observed_ids
            result.execution_ids = observed_ids
        result.execution_agents = sorted(
            {
                str(row.get("agent_id"))
                for row in executions
                if isinstance(row.get("agent_id"), str) and str(row.get("agent_id")).strip()
            }
        )
        root = next(
            (
                row
                for row in executions
                if str(row.get("execution_id") or "") == result.root_execution_id
            ),
            None,
        )
        if root is not None:
            result.root_status = str(root.get("status") or "unknown").casefold()
        gates.append(
            Gate(
                "partial.executions_captured",
                True,
                f"executions={len(executions)}",
                case_id,
                latency,
            )
        )
    except Exception as error:
        gates.append(Gate("partial.executions_captured", False, str(error), case_id))

    try:
        events, latency = fetch_events(
            client, result.task_id, execution_ids, started_at_ms
        )
        event_tool_calls, event_paired = tool_calls_from_events(events)
        result.tool_calls = event_tool_calls
        result.phase_timings_ms = phase_timings_from_events(events)
        gates.append(
            Gate(
                "partial.events_captured",
                True,
                f"events={len(events)} journal_tool_finishes={len(event_tool_calls)} paired={event_paired}",
                case_id,
                latency,
            )
        )
    except Exception as error:
        gates.append(Gate("partial.events_captured", False, str(error), case_id))

    try:
        fact_calls, paired, latency = query_tool_calls(
            client,
            result.task_id,
            execution_ids,
            started_at_ms,
            int(time.time() * 1000),
        )
        if fact_calls:
            result.tool_calls = fact_calls
        gates.append(
            Gate(
                "partial.tool_facts_captured",
                True,
                f"tool_finishes={len(fact_calls)} paired={paired}",
                case_id,
                latency,
            )
        )
    except Exception as error:
        gates.append(Gate("partial.tool_facts_captured", False, str(error), case_id))

    try:
        result.llm_calls, latency = query_llm_calls(
            client,
            result.task_id,
            execution_ids,
            started_at_ms,
            int(time.time() * 1000),
        )
        gates.append(
            Gate(
                "partial.telemetry_captured",
                True,
                f"calls={len(result.llm_calls)}",
                case_id,
                latency,
            )
        )
    except Exception as error:
        gates.append(Gate("partial.telemetry_captured", False, str(error), case_id))


def unrecovered_tool_failures(
    calls: list[ToolCall], *, answer_supported: bool = False
) -> tuple[list[ToolCall], int]:
    """Keep failed attempts diagnostic when later/grounded evidence recovers.

    A failed page read is not a failed answer when another page was opened and
    the independent opened-page judge found the emitted answer supported. This
    is intentionally narrower than forgiving every retrieval failure: search
    and non-retrieval tools still require a later same-tool success.
    """
    recoverable_tools = {"content_search", "content_read"}
    any_successful_read = any(
        call.success and call.tool_name == "content_read" for call in calls
    )
    unrecovered: list[ToolCall] = []
    recovered = 0
    for index, call in enumerate(calls):
        if call.success:
            continue
        later_recovery = call.tool_name in recoverable_tools and any(
            later.success and later.tool_name == call.tool_name
            for later in calls[index + 1 :]
        )
        grounded_alternate = (
            call.tool_name == "content_read"
            and answer_supported
            and any_successful_read
        )
        if later_recovery or grounded_alternate:
            recovered += 1
        else:
            unrecovered.append(call)
    return unrecovered, recovered


def task_summary_needs_llm(text: str) -> bool:
    """Mirror the product's cheap long/noisy task-summary gate."""
    trimmed = text.strip()
    length = len(trimmed)
    if length > 250:
        return True
    if not trimmed:
        return False
    non_alnum = sum(
        1 for char in trimmed if not char.isalnum() and not char.isspace()
    )
    return non_alnum / length > 0.35


def continuation_growth_gate(result: CaseResult) -> Gate:
    """Bound marginal context growth while allowing periodic re-bootstrap.

    Provider usage can include the retained server-side prefix, so successive
    calls need not get smaller. The useful invariant is that ordinary stateful
    turns add a bounded delta rather than another full bootstrap. Only a call
    explicitly marked `prompt_mode=rebootstrap` in canonical correlation
    telemetry is excluded from this slope check.
    """
    by_execution: dict[str, list[dict[str, Any]]] = {}
    for call in result.llm_calls:
        if (
            str(call.get("operation") or "") != "agentic_decision"
            or value_is_false(call.get("success"))
        ):
            continue
        execution_id = str(call.get("execution_id") or "unscoped")
        tokens = int(call.get("input_tokens") or 0)
        if tokens > 0:
            by_execution.setdefault(execution_id, []).append(call)

    breaches: list[dict[str, Any]] = []
    slopes: dict[str, list[int]] = {}
    for execution_id, calls in by_execution.items():
        if len(calls) < 2:
            continue
        values = [int(call.get("input_tokens") or 0) for call in calls]
        threshold = max(8_000, int(values[0] * 0.60))
        # Provider token accounting is integer-estimated and can move slightly
        # when a tool result crosses a tokenizer boundary. Keep the structural
        # ceiling with a small five-percent allowance (and a 256-token floor)
        # while still rejecting anything resembling a repeated bootstrap.
        tolerance = max(256, int(threshold * 0.05))
        effective_threshold = threshold + tolerance
        deltas: list[int] = []
        for index, (previous, current) in enumerate(zip(values, values[1:]), start=1):
            current_iteration_id = str(calls[index].get("iteration_id") or "")
            prompt_projection_mode = str(calls[index].get("prompt_projection_mode") or "")
            if prompt_projection_mode == "rebootstrap" or current_iteration_id.endswith(
                ":prompt_mode=rebootstrap"
            ):
                continue
            delta = max(0, current - previous)
            deltas.append(delta)
            if delta > effective_threshold:
                breaches.append(
                    {
                        "execution_id": execution_id,
                        "call_index": index + 1,
                        "delta": delta,
                        "threshold": threshold,
                        "tolerance": tolerance,
                        "effective_threshold": effective_threshold,
                    }
                )
        slopes[execution_id] = deltas
    return Gate(
        "telemetry.continuation_delta_growth_bounded",
        not breaches,
        f"marginal_input_tokens={slopes} breaches={breaches}",
        result.case_id,
    )


def cache_usage_accounting_gate(result: CaseResult) -> Gate:
    """Reject provider cache buckets that exceed their reported input total."""
    breaches: list[dict[str, Any]] = []
    observed = 0
    for call in result.llm_calls:
        if value_is_false(call.get("success")) or call.get("input_tokens") is None:
            continue
        input_tokens = int(call.get("input_tokens") or 0)
        cache_read = int(call.get("cache_read_tokens") or 0)
        cache_creation = int(call.get("cache_creation_tokens") or 0)
        if cache_read or cache_creation:
            observed += 1
        if min(input_tokens, cache_read, cache_creation) < 0 or (
            cache_read + cache_creation > input_tokens
        ):
            breaches.append(
                {
                    "operation": str(call.get("operation") or ""),
                    "profile": str(call.get("profile") or ""),
                    "input_tokens": input_tokens,
                    "cache_read_tokens": cache_read,
                    "cache_creation_tokens": cache_creation,
                }
            )
    return Gate(
        "telemetry.cache_usage_accounting_valid",
        not breaches,
        f"cache_observations={observed} breaches={breaches}",
        result.case_id,
    )


def lifecycle_gates(result: CaseResult, answer_text: str) -> list[Gate]:
    """Verify answer-ready and auxiliary work remain separate lifecycles."""
    case_id = result.case_id
    # An execution that ran the agentic loop must end it exactly once. An
    # execution that never ran one — an explicit-delegation root projects its
    # child's result inline — has no agentic terminal to count; its lifecycle
    # terminal (`execution.completed` / `.failed`) stands in, and there must be
    # exactly one of those instead.
    loop_ran = {
        execution_id: any(
            event.get("execution_id") == execution_id
            and str(event.get("event_type") or "").startswith("agentic.")
            for event in result.settled_events
        )
        for execution_id in result.execution_ids
    }
    completion_counts = {
        execution_id: sum(
            (
                event.get("event_type") in AGENTIC_TERMINAL_EVENT_TYPES
                if loop_ran[execution_id]
                else event.get("event_type") in LIFECYCLE_TERMINAL_EVENT_TYPES
            )
            and event.get("execution_id") == execution_id
            for event in result.terminal_lifecycle_events
        )
        for execution_id in result.execution_ids
    }
    summary_calls = [
        call
        for call in result.llm_calls
        if str(call.get("operation") or "") == "task_summary"
    ]
    summary_expected = task_summary_needs_llm(answer_text)
    expected_summary_calls = 1 if summary_expected else 0
    summary_timestamps = [
        int(call.get("timestamp_ms") or 0)
        for call in summary_calls
        if int(call.get("timestamp_ms") or 0) > 0
    ]
    late_terminals = []
    if summary_timestamps:
        first_summary_at = min(summary_timestamps)
        late_terminals = [
            event
            for event in result.terminal_lifecycle_events
            if isinstance(event.get("timestamp_ms"), int)
            and int(event["timestamp_ms"]) > first_summary_at
        ]
    root_mismatches = [
        {
            "execution_id": call.get("execution_id"),
            "root_execution_id": call.get("root_execution_id"),
        }
        for call in summary_calls
        if str(call.get("root_execution_id") or "") != result.root_execution_id
    ]
    background_failures = [
        str(call.get("operation") or "")
        for call in result.llm_calls
        if str(call.get("operation") or "")
        in {
            "task_summary",
            "learning_reflection",
            "memory_episode_quality_classification",
            "evidence_precision_judge",
        }
        and value_is_false(call.get("success"))
    ]
    gates = [
        Gate(
            "lifecycle.one_agentic_terminal_per_execution",
            bool(completion_counts)
            and all(count == 1 for count in completion_counts.values()),
            f"counts={completion_counts}",
            case_id,
        ),
        Gate(
            "lifecycle.background_operations_nonblocking",
            result.root_status == "completed" and bool(result.output_id),
            "answer was observed before telemetry settling; "
            f"background_failures_diagnostic_only={background_failures}",
            case_id,
        ),
    ]
    llm_telemetry_inconclusive = any(
        diagnostic.startswith("llm_telemetry:")
        for diagnostic in result.infrastructure_inconclusive
    )
    if llm_telemetry_inconclusive:
        gates.append(
            Gate(
                "lifecycle.telemetry_infrastructure_inconclusive",
                True,
                "summary, attribution, continuation-growth, and cache-accounting "
                "checks were not scored because governed telemetry stayed busy",
                case_id,
            )
        )
        return gates
    gates.extend(
        [
            Gate(
                "lifecycle.task_summary_once_per_output_revision",
                len(summary_calls) == expected_summary_calls,
                f"expected={expected_summary_calls} observed={len(summary_calls)} "
                f"llm_summary_required={summary_expected}",
                case_id,
            ),
            Gate(
                "lifecycle.task_summary_root_attributed",
                not root_mismatches,
                f"root={result.root_execution_id} mismatches={root_mismatches}",
                case_id,
            ),
            Gate(
                "lifecycle.no_terminal_replay_after_task_summary",
                not late_terminals,
                f"late_terminal_events={late_terminals}",
                case_id,
            ),
            continuation_growth_gate(result),
            cache_usage_accounting_gate(result),
        ]
    )
    return gates


def normalize_term_text(value: str) -> str:
    """Normalize presentation typography before semantic fixture matching."""
    return (
        unicodedata.normalize("NFKC", value)
        .translate(TERM_NORMALIZATION_TRANSLATION)
        .casefold()
    )



def assign_verdict(case: dict[str, Any], result: CaseResult, case_gates: list[Gate]) -> Gate:
    """The two-half verdict, over EVERY gate the case accumulated.

    Assertion gates fail the case. Coverage gates decide full versus partial —
    and only when the runtime itself said the terminal was partial; a terminal
    that claimed to be full and did not cover the request is a failure, exactly
    as before.
    """
    assertion_failed = any(not gate.passed and not gate.coverage for gate in case_gates)
    coverage_complete = all(gate.passed for gate in case_gates if gate.coverage)
    if assertion_failed:
        result.verdict = "fail"
    elif result.completion_kind == "partial":
        if bool(case.get("require_full")):
            result.verdict = "fail"
        elif result.honesty == "resolvable":
            result.verdict = "weak_partial"
        else:
            result.verdict = "partial_pass"
    elif coverage_complete:
        result.verdict = "full_pass"
    else:
        result.verdict = "fail"
    return Gate(
        "outcome.verdict",
        result.verdict != "fail",
        f"verdict={result.verdict} completion_kind={result.completion_kind} "
        f"open_items={len(result.open_items)} honesty={result.honesty or '-'}"
        + (f" ({result.honesty_reason})" if result.honesty_reason else ""),
        result.case_id,
    )

def score_case(
    case: dict[str, Any],
    result: CaseResult,
    paired_tool_calls: int,
    llm_profile: str = DEFAULT_EVAL_LLM_PROFILE,
) -> list[Gate]:
    case_id = result.case_id
    text_folded = normalize_term_text(result.answer_excerpt)
    citations = result.citations
    hosts = {hostname(url) for url in citations if hostname(url)}
    domain_failures: list[list[str]] = []
    for group in case["required_domain_groups"]:
        if not any(host_matches(host, expected) for host in hosts for expected in group):
            domain_failures.append(group)
    term_failures: list[list[str]] = []
    for group in case["required_term_groups"]:
        if not any(normalize_term_text(term) in text_folded for term in group):
            term_failures.append(group)
    resolved = sum(
        1
        for probe in result.citation_probes
        if probe.status is not None and 200 <= probe.status < 400
    )
    unrecovered_tools, recovered_tool_attempts = unrecovered_tool_failures(
        result.tool_calls,
        answer_supported=result.judge_supported is True,
    )
    failed_tools = len(unrecovered_tools)
    called_tools = {call.tool_name for call in result.tool_calls if call.success}
    missing_tool_groups = [
        group
        for group in case.get("required_tool_groups", [])
        if not any(tool in called_tools for tool in group)
    ]
    background_operations = {
        "task_summary",
        "learning_reflection",
        "memory_episode_quality_classification",
        "evidence_precision_judge",
    }
    failed_llm = sum(
        1
        for call in result.llm_calls
        if value_is_false(call.get("success"))
        and str(call.get("operation") or "") not in background_operations
    )
    selected_profile_decisions = [
        call
        for call in result.llm_calls
        if str(call.get("operation") or "") == "agentic_decision"
        and str(call.get("profile") or "") == llm_profile
        and not value_is_false(call.get("success"))
    ]
    other_profile_decisions = [
        call
        for call in result.llm_calls
        if str(call.get("operation") or "") == "agentic_decision"
        and str(call.get("profile") or "") != llm_profile
        and not value_is_false(call.get("success"))
    ]
    researcher_count = sum(
        1 for agent in result.execution_agents if agent == WEB_RESEARCHER_AGENT_ID
    )
    min_answer_chars = int(case["min_answer_chars"])
    semantic_and_citation_complete = (
        result.judge_supported is True
        and not term_failures
        and len(citations) >= int(case["min_citations"])
        and len(hosts) >= int(case["min_distinct_domains"])
        and not domain_failures
        and resolved >= int(case["min_resolvable_citations"])
    )
    minimum_content_passed = (
        result.answer_chars >= min_answer_chars or semantic_and_citation_complete
    )
    gates = [
        Gate(
            "execution.completed",
            result.root_status == "completed",
            f"root_status={result.root_status}",
            case_id,
        ),
        Gate(
            "execution.web_researcher_lineage",
            researcher_count >= 1,
            f"agents={result.execution_agents}",
            case_id,
        ),
        Gate(
            "answer.minimum_content",
            minimum_content_passed,
            f"chars={result.answer_chars} min={min_answer_chars} "
            f"semantic_citation_override={semantic_and_citation_complete}",
            case_id,
            coverage=True,
        ),
        Gate(
            "answer.bounded_words",
            result.answer_words <= int(case["max_answer_words"]),
            f"words={result.answer_words} max={case['max_answer_words']}",
            case_id,
        ),
        Gate(
            "answer.required_terms",
            not term_failures,
            f"missing_groups={term_failures}",
            case_id,
            coverage=True,
        ),
        Gate(
            "citations.minimum",
            len(citations) >= int(case["min_citations"]),
            f"citations={len(citations)} min={case['min_citations']}",
            case_id,
            coverage=True,
        ),
        Gate(
            "citations.distinct_domains",
            len(hosts) >= int(case["min_distinct_domains"]),
            f"domains={sorted(hosts)} min={case['min_distinct_domains']}",
            case_id,
            coverage=True,
        ),
        Gate(
            "citations.required_authorities",
            not domain_failures,
            f"missing_groups={domain_failures}",
            case_id,
            coverage=True,
        ),
        Gate(
            "citations.reachable",
            resolved >= int(case["min_resolvable_citations"]),
            f"resolved={resolved} required={case['min_resolvable_citations']}",
            case_id,
            sum(probe.latency_ms for probe in result.citation_probes),
            coverage=True,
        ),
        Gate(
            "answer.supported_by_opened_pages",
            # The runtime's own rule, mirrored: an assertion the source
            # CONTRADICTS fails; an assertion merely ABSENT from the source
            # on a terminal the runtime marked partial is the caveat that
            # partial already carries, graded for honesty below, not a
            # failure. A full terminal still needs every assertion supported.
            result.judge_supported is True
            or (
                result.completion_kind == "partial"
                and result.judge_supported is False
                and result.judge_unsupported_kind == "absent"
            ),
            (result.judge_reason or "evidence judge did not return a verdict")
            + (
                f" [unsupported_kind={result.judge_unsupported_kind}]"
                if result.judge_unsupported_kind
                else ""
            ),
            case_id,
            result.judge_latency_ms,
        ),
        Gate(
            "tools.paired_lifecycle",
            paired_tool_calls >= int(case["min_paired_tool_calls"]),
            f"paired={paired_tool_calls} required={case['min_paired_tool_calls']}",
            case_id,
        ),
        Gate(
            "tools.no_unrecovered_failures",
            failed_tools <= int(case["max_failed_tool_calls"]),
            "unrecovered="
            + str([(call.tool_name, call.error) for call in unrecovered_tools])
            + f" recovered_attempts={recovered_tool_attempts} max={case['max_failed_tool_calls']}",
            case_id,
        ),
        Gate(
            "tools.required_vector_capability",
            not missing_tool_groups,
            f"called={sorted(called_tools)} missing_groups={missing_tool_groups}",
            case_id,
        ),
    ]
    llm_telemetry_inconclusive = any(
        diagnostic.startswith("llm_telemetry:")
        for diagnostic in result.infrastructure_inconclusive
    )
    if llm_telemetry_inconclusive:
        gates.append(
            Gate(
                "telemetry.infrastructure_inconclusive",
                True,
                "LLM telemetry gates were not scored because governed analytics "
                "remained busy after bounded retries",
                case_id,
            )
        )
    else:
        gates.extend(
            [
                Gate(
                    "telemetry.linked_llm_calls",
                    bool(result.llm_calls),
                    f"calls={len(result.llm_calls)}",
                    case_id,
                ),
                Gate(
                    "telemetry.no_failed_llm_calls",
                    failed_llm <= int(case["max_failed_llm_calls"]),
                    f"failed={failed_llm} max={case['max_failed_llm_calls']}",
                    case_id,
                ),
                Gate(
                    "telemetry.eval_decision_profile_observed",
                    bool(selected_profile_decisions),
                    "successful agentic_decision profiles="
                    + str(
                        sorted(
                            {
                                str(call.get("profile") or "")
                                for call in result.llm_calls
                                if str(call.get("operation") or "")
                                == "agentic_decision"
                                and not value_is_false(call.get("success"))
                            }
                        )
                    ),
                    case_id,
                ),
                Gate(
                    "telemetry.eval_decisions_only_use_selected_profile",
                    not other_profile_decisions,
                    f"selected={llm_profile} other successful agentic_decision profiles="
                    + str(
                        sorted(
                            {
                                str(call.get("profile") or "<unset>")
                                for call in other_profile_decisions
                            }
                        )
                    ),
                    case_id,
                ),
            ]
        )
    if case["mode"] == "delegated":
        gates.append(
            Gate(
                "delegation.parent_and_child_lineage",
                result.root_agent_id != WEB_RESEARCHER_AGENT_ID
                and WEB_RESEARCHER_AGENT_ID in result.execution_agents
                and len(result.execution_agents) >= 2,
                f"root={result.root_agent_id} agents={result.execution_agents}",
                case_id,
            )
        )
    return gates


def run_case(
    client: Client,
    case: dict[str, Any],
    repeat: int,
    citation_probe_limit: int,
    llm_profile: str = DEFAULT_EVAL_LLM_PROFILE,
    delete_tasks: bool = False,
) -> tuple[CaseResult, list[Gate]]:
    fixture_id = str(case["id"])
    case_id = f"{fixture_id}#{repeat}"
    result = CaseResult(
        case_id=case_id,
        fixture_id=fixture_id,
        repeat=repeat,
        mode=str(case["mode"]),
        root_agent_id=str(case["root_agent_id"]),
    )
    gates: list[Gate] = []
    started_at_ms = int(time.time() * 1000)
    wall_started = time.perf_counter()
    paired_tool_calls = 0
    try:
        created, create_latency, _ = client.json(
            "POST",
            "/api/magician/v3/tasks",
            {
                "title": f"[eval/web-researcher] {case.get('title') or fixture_id}",
                "description": str(case["query"]),
                "agent_id": result.root_agent_id,
                "approved": True,
                "created_by": "user",
            },
            expected=(201,),
        )
        result.task_id = task_id_from_response(created)
        gates.append(
            Gate("task.created", True, f"task={result.task_id}", case_id, create_latency)
        )
        accepted, execute_latency, _ = client.json(
            "POST",
            f"/api/magician/v3/tasks/{quote(result.task_id)}/execute",
            eval_execution_request(
                llm_profile,
                WEB_RESEARCHER_AGENT_ID if case["mode"] == "delegated" else None,
            ),
            expected=(202,),
        )
        result.root_execution_id = execution_id_from_response(accepted)
        gates.append(
            Gate(
                "task.execution_accepted",
                True,
                f"execution={result.root_execution_id}",
                case_id,
                execute_latency,
            )
        )
        executions, state, _ = wait_for_answer(
            client,
            result.task_id,
            result.root_execution_id,
        )
        result.answer_ready_ms = (time.perf_counter() - wall_started) * 1000
        result.answer_observed_at_ms = int(time.time() * 1000)
        root = next(
            row
            for row in executions
            if str(row.get("execution_id") or "") == result.root_execution_id
        )
        result.root_status = str(root.get("status") or "unknown").casefold()
        result.execution_ids = [
            str(row.get("execution_id"))
            for row in executions
            if isinstance(row.get("execution_id"), str)
        ]
        result.execution_agents = sorted(
            {
                str(row.get("agent_id"))
                for row in executions
                if isinstance(row.get("agent_id"), str) and str(row.get("agent_id")).strip()
            }
        )
        # The runtime's own verdict on what it delivered. `completion_kind`
        # is propagated from the yield through the child, the root, and the
        # task; a partial here is the runtime saying so, not the eval guessing.
        completion_kind = state.get("completion_kind")
        open_items = state.get("open_items")
        if not isinstance(completion_kind, str):
            researcher_rows = [
                row
                for row in executions
                if str(row.get("agent_id") or "") == WEB_RESEARCHER_AGENT_ID
                and isinstance(row.get("completion_kind"), str)
            ]
            if researcher_rows:
                completion_kind = researcher_rows[-1].get("completion_kind")
                open_items = researcher_rows[-1].get("open_items")
        result.completion_kind = completion_kind if isinstance(completion_kind, str) else None
        result.open_items = (
            [str(item) for item in open_items if str(item).strip()]
            if isinstance(open_items, list)
            else []
        )
        primary_output_id = str(state.get("primary_user_output_id") or "")
        output_id, media_type, raw_output, output_latency = fetch_primary_output(
            client, result.task_id, primary_output_id
        )
        result.output_id = output_id
        result.output_media_type = media_type
        answer_text, citations = text_and_citations(raw_output, media_type)
        result.answer_chars = len(answer_text)
        result.answer_words = len(re.findall(r"\b\w+\b", answer_text))
        result.answer_excerpt = answer_text[:MAX_ANSWER_REPORT_CHARS]
        result.citations = citations
        gates.append(
            Gate(
                "output.primary_user_output_read",
                bool(answer_text),
                f"output={output_id} media_type={media_type}",
                case_id,
                output_latency,
            )
        )

        events, event_latency = fetch_events(
            client, result.task_id, result.execution_ids, started_at_ms
        )
        event_tool_calls, event_paired_tool_calls = tool_calls_from_events(events)
        result.tool_calls = event_tool_calls
        paired_tool_calls = event_paired_tool_calls
        gates.append(
            Gate(
                "events.backfill_read",
                True,
                f"events={len(events)} journal_tool_finishes={len(event_tool_calls)}",
                case_id,
                event_latency,
            )
        )

        ended_at_ms = int(time.time() * 1000)
        try:
            fact_calls, fact_paired, tool_fact_latency = wait_for_tool_calls(
                client,
                result.task_id,
                result.execution_ids,
                started_at_ms,
                ended_at_ms,
            )
            if fact_calls:
                result.tool_calls = fact_calls
                paired_tool_calls = fact_paired
            gates.append(
                Gate(
                    "tool_facts.query_completed",
                    True,
                    f"tool_finishes={len(fact_calls)} paired={fact_paired}",
                    case_id,
                    tool_fact_latency,
                )
            )
        except AnalyticsInfrastructureInconclusive as error:
            result.infrastructure_inconclusive.append(f"tool_facts: {error}")
            gates.append(
                Gate(
                    "tool_facts.infrastructure_inconclusive",
                    True,
                    str(error),
                    case_id,
                )
            )

        # Probe distinct citations in emitted order. Reachability is quality
        # evidence, but it is deliberately outside answer_ready_ms.
        for url in citations[: max(0, citation_probe_limit)]:
            result.citation_probes.append(verify_citation(url))

        try:
            own_sources = own_opened_evidence(
                client, result.task_id, result.execution_ids, started_at_ms
            )
        except Exception:
            own_sources = []
        observed_actions = tool_inventory_lines(result.tool_calls)
        try:
            (
                result.judge_supported,
                result.judge_reason,
                result.judge_latency_ms,
                result.declared_open,
            ) = judge_answer(
                client,
                str(case["query"]),
                answer_text,
                result.citation_probes,
                llm_profile,
                own_sources,
                observed_actions,
            )
            result.judge_unsupported_kind = LAST_JUDGE_UNSUPPORTED_KIND[0]
        except Exception as error:
            result.judge_supported = False
            result.judge_reason = f"evidence judge failed: {error}"

        if result.completion_kind == "partial" and result.open_items:
            try:
                result.honesty, result.honesty_reason = judge_declared_open_honesty(
                    client,
                    str(case["query"]),
                    result.open_items,
                    own_sources,
                    llm_profile,
                )
            except Exception as error:
                result.honesty = "unchecked"
                result.honesty_reason = f"honesty judge failed: {error}"

        try:
            result.llm_calls, telemetry_latency = wait_for_llm_calls(
                client,
                result.task_id,
                result.execution_ids,
                started_at_ms,
                ended_at_ms,
                {"task_summary"} if task_summary_needs_llm(answer_text) else set(),
            )
            gates.append(
                Gate(
                    "telemetry.query_completed",
                    True,
                    f"calls={len(result.llm_calls)}",
                    case_id,
                    telemetry_latency,
                )
            )
        except AnalyticsInfrastructureInconclusive as error:
            result.infrastructure_inconclusive.append(f"llm_telemetry: {error}")
            gates.append(
                Gate(
                    "telemetry.infrastructure_inconclusive",
                    True,
                    str(error),
                    case_id,
                )
            )
        settled_events, settled_event_latency = fetch_events(
            client, result.task_id, result.execution_ids, started_at_ms
        )
        # The agentic terminal is mirrored into the journal by the runtime
        # bridge a few seconds after the execution completes; a read taken
        # the instant the task settles can precede it. Settle briefly rather
        # than score a race.
        for _ in range(8):
            loop_executions = {
                str(event.get("execution_id") or "")
                for event in settled_events
                if str(event.get("event_type") or "").startswith("agentic.")
            }
            terminal_executions = {
                str(event.get("execution_id") or "")
                for event in settled_events
                if event.get("event_type") in AGENTIC_TERMINAL_EVENT_TYPES
            }
            if loop_executions <= terminal_executions:
                break
            time.sleep(3)
            settled_events, extra_latency = fetch_events(
                client, result.task_id, result.execution_ids, started_at_ms
            )
            settled_event_latency += extra_latency
        result.settled_events = settled_events
        result.terminal_lifecycle_events = terminal_lifecycle_events(settled_events)
        result.phase_timings_ms = phase_timings_from_events(settled_events)
        gates.append(
            Gate(
                "events.lifecycle_settled",
                True,
                f"events={len(settled_events)} "
                f"terminal={len(result.terminal_lifecycle_events)}",
                case_id,
                settled_event_latency,
            )
        )
        gates.extend(lifecycle_gates(result, answer_text))
        gates.extend(score_case(case, result, paired_tool_calls, llm_profile))
        gates.append(
            assign_verdict(
                case, result, [gate for gate in gates if gate.case_id == result.case_id]
            )
        )
    except KeyboardInterrupt:
        result.interrupted = True
        result.error = "stopped by operator"
        result.answer_ready_ms = (time.perf_counter() - wall_started) * 1000
        capture_partial_case_observability(client, result, gates, started_at_ms)
        gates.append(
            Gate(
                "case.operator_stopped",
                False,
                "Ctrl-C received; active execution cancelled during cleanup",
                case_id,
                result.answer_ready_ms,
            )
        )
    except Exception as error:
        result.error = str(error)
        result.answer_ready_ms = (time.perf_counter() - wall_started) * 1000
        capture_partial_case_observability(client, result, gates, started_at_ms)
        gates.append(Gate("case.completed", False, str(error), case_id, result.answer_ready_ms))
    finally:
        if result.task_id:
            # **The task is KEPT by default.** Deleting it destroys the only
            # copy of the run's decision events, and those are what a failure
            # investigation needs — the report keeps gates, tool and LLM rows,
            # but not the per-iteration reasoning that says WHY a run went the
            # way it did. Several investigations here lost exactly that to
            # cleanup and had to be reproduced by hand.
            #
            # `--delete-tasks` restores the old hygiene for anyone running the
            # suite in a loop who does not want the store to grow.
            if delete_tasks:
                try:
                    removed, detail, latency = cleanup_task(
                        client, result.task_id, result.root_execution_id or None
                    )
                    gates.append(Gate("cleanup.task_removed", removed, detail, case_id, latency))
                except Exception as error:
                    gates.append(Gate("cleanup.task_removed", False, str(error), case_id))
            else:
                # Retained, but never left running: an interrupted or
                # non-terminal run is cancelled first, so keeping the evidence
                # does not also keep spending on it.
                detail = f"task={result.task_id} kept for inspection (--delete-tasks to remove)"
                if result.root_execution_id and result.root_status not in (
                    "completed",
                    "failed",
                    "cancelled",
                ):
                    try:
                        cancelled, cancel_detail, _ = cancel_active_execution(
                            client, result.root_execution_id
                        )
                        detail = f"{detail}; active execution cancelled ({cancel_detail})"
                        if not cancelled:
                            detail = f"{detail} [cancel not confirmed]"
                    except Exception as cancel_error:
                        detail = f"{detail}; cancel failed: {cancel_error}"
                gates.append(Gate("cleanup.task_retained", True, detail, case_id))
    return result, gates


def validate_agent_contract(cases: list[dict[str, Any]]) -> list[Gate]:
    researcher_path = (
        REPO_ROOT
        / "magician_data_v3/system/agent_templates/agents/web-researcher/definition.agent.yaml"
    )
    personal_path = (
        REPO_ROOT
        / "magician_data_v3/system/agent_templates/agents/personal-assistant/definition.agent.yaml"
    )
    researcher = researcher_path.read_text(encoding="utf-8") if researcher_path.is_file() else ""
    personal = personal_path.read_text(encoding="utf-8") if personal_path.is_file() else ""
    researcher_tools = declared_agent_tools(researcher)
    root_agents = {str(case["root_agent_id"]) for case in cases}
    return [
        Gate(
            "contract.web_researcher_definition",
            "agent_id: web-researcher" in researcher,
            str(researcher_path),
        ),
        Gate(
            "contract.focused_tool_boundary",
            researcher_tools == EXPECTED_WEB_RESEARCHER_TOOLS,
            "missing="
            + str(sorted(EXPECTED_WEB_RESEARCHER_TOOLS - researcher_tools))
            + " unexpected="
            + str(sorted(researcher_tools - EXPECTED_WEB_RESEARCHER_TOOLS)),
        ),
        Gate(
            "contract.retired_tools_absent",
            not (researcher_tools & RETIRED_WEB_RESEARCHER_TOOLS),
            f"retired={sorted(researcher_tools & RETIRED_WEB_RESEARCHER_TOOLS)}",
        ),
        Gate(
            "contract.bounded_luna_decision_profile",
            "agentic_decision:\n      profile: gpt6luna-responses-toolsany" in researcher,
            "web-researcher agentic_decision uses governed Luna medium reasoning",
        ),
        Gate(
            "contract.bounded_depth_prompt",
            bounded_research_prompt_contract(researcher),
            "bounded-by-default, gist-first, stop-when-sufficient prompt without legacy full-read quota",
        ),
        Gate(
            "contract.delegating_parent",
            "personal-assistant" not in root_agents
            or (
                "agent_id: personal-assistant" in personal
                and "delegation_targets:" in personal
                and ("- '*'" in personal or "- web-researcher" in personal)
            ),
            "personal-assistant can reach web-researcher",
        ),
    ]


def bounded_research_prompt_contract(persona: str) -> bool:
    return all(
        marker in persona
        for marker in (
            "The default is bounded research",
            "Start reads at `depth: gist`",
            "Synthesize as soon",
            "normally 2-6 essential words",
            "Search snippets choose pages",
            "fetch_status: complete",
            "Never combine a factual value from a search snippet",
            "A failed source is an internal attempt",
            "fully closes the evidence gap",
            "Before yielding, reconcile every material final claim",
            "recommendation or ranking",
            "bounded answer into a report",
            "sources solely to hit a count",
            '"common":{"fresh":true,"limit":5}',
        )
    ) and all(
        legacy not in persona
        for legacy in (
            "Spend ~50% on reading full articles",
            "Structure your final answer as:",
        )
    )


def validate_runtime_agent_contract(client: Client) -> list[Gate]:
    payload, latency, _ = client.json(
        "GET", f"/api/magician/v2/agents/{WEB_RESEARCHER_AGENT_ID}"
    )
    definition = payload.get("definition") if isinstance(payload, dict) else None
    if not isinstance(definition, dict):
        return [
            Gate(
                "runtime_contract.web_researcher_definition",
                False,
                f"invalid scoped definition response: {payload}",
                latency_ms=latency,
            )
        ]
    tools = {
        str(tool)
        for tool in definition.get("tools", [])
        if isinstance(tool, str) and tool.strip()
    }
    llm_routing = definition.get("llm_routing")
    operations = llm_routing.get("operations") if isinstance(llm_routing, dict) else None
    agentic_decision = (
        operations.get("agentic_decision") if isinstance(operations, dict) else None
    )
    profile = (
        str(agentic_decision.get("profile") or "")
        if isinstance(agentic_decision, dict)
        else ""
    )
    persona = str(definition.get("persona") or "")
    return [
        Gate(
            "runtime_contract.web_researcher_definition",
            str(definition.get("agent_id") or "") == WEB_RESEARCHER_AGENT_ID,
            f"scope agent={definition.get('agent_id')}",
            latency_ms=latency,
        ),
        Gate(
            "runtime_contract.focused_tool_boundary",
            tools == EXPECTED_WEB_RESEARCHER_TOOLS,
            "missing="
            + str(sorted(EXPECTED_WEB_RESEARCHER_TOOLS - tools))
            + " unexpected="
            + str(sorted(tools - EXPECTED_WEB_RESEARCHER_TOOLS)),
        ),
        Gate(
            "runtime_contract.retired_tools_absent",
            not (tools & RETIRED_WEB_RESEARCHER_TOOLS),
            f"retired={sorted(tools & RETIRED_WEB_RESEARCHER_TOOLS)}",
        ),
        Gate(
            "runtime_contract.bounded_luna_decision_profile",
            profile == "gpt6luna-responses-toolsany",
            f"profile={profile or '<unset>'}",
        ),
        Gate(
            "runtime_contract.bounded_depth_prompt",
            bounded_research_prompt_contract(persona),
            "scoped persona carries bounded/gist/stop rules and excludes legacy full-read/report mandates",
        ),
    ]


def synthetic_self_test(
    cases: list[dict[str, Any]],
    llm_profile: str = DEFAULT_EVAL_LLM_PROFILE,
) -> tuple[list[CaseResult], list[Gate]]:
    sample = (
        b"<html><body><h1>Current Python release</h1><p>Python 3.14 is the latest "
        b"stable release.</p><a href='https://www.python.org/downloads/'>Official "
        b"downloads</a></body></html>"
    )
    text, citations = text_and_citations(sample, "text/html")
    events = [
        {"event_type": "tool.call.started", "data": {"call_id": "c1", "tool_name": "content_search"}},
        {"event_type": "tool.call.finished", "data": {"call_id": "c1", "tool_name": "content_search", "success": True, "duration_ms": 12}},
    ]
    tools, paired = tool_calls_from_events(events)
    result = CaseResult(
        case_id="self_test#1",
        fixture_id="self_test",
        repeat=1,
        mode="direct",
        root_agent_id=WEB_RESEARCHER_AGENT_ID,
        root_status="completed",
        answer_ready_ms=250,
        answer_chars=len(text),
        answer_words=len(text.split()),
        answer_excerpt=text,
        citations=citations,
        judge_supported=True,
        judge_reason="self-test source supports the synthetic answer",
        tool_calls=tools,
        llm_calls=[
            {
                "success": True,
                "operation": "agentic_decision",
                "profile": llm_profile,
                "provider": "self-test",
                "model": "fixture",
                "cost_usd": 0,
            }
        ],
        execution_agents=[WEB_RESEARCHER_AGENT_ID],
    )
    gates = [
        *validate_agent_contract(cases),
        Gate("routing.eval_profile", True, llm_profile),
        Gate("self_test.html_text", "Current Python release" in text, text),
        Gate("self_test.citation", citations == ["https://www.python.org/downloads/"], str(citations)),
        Gate("self_test.tool_pair", paired == 1 and tools[0].success, f"paired={paired}"),
        Gate("self_test.fixture_count", len(cases) >= 2, f"cases={len(cases)}"),
    ]
    return [result], gates


def report_payload(
    mode: str,
    fixture_path: Path,
    results: list[CaseResult],
    gates: list[Gate],
) -> dict[str, Any]:
    durations = [result.answer_ready_ms for result in results if result.answer_ready_ms > 0]
    llm_calls = [call for result in results for call in result.llm_calls]
    tool_calls = [call for result in results for call in result.tool_calls]
    probes = [probe for result in results for probe in result.citation_probes]
    inconclusive_cases = {
        result.case_id for result in results if result.infrastructure_inconclusive
    }
    quality_passed_cases = {
        result.case_id
        for result in results
        if all(
            gate.passed
            for gate in gates
            if gate.case_id == result.case_id
            # On a partial terminal, coverage gates report; they do not fail.
            and not (gate.coverage and result.completion_kind == "partial")
        )
    }
    verdict_counts = {
        verdict: sum(result.verdict == verdict for result in results)
        for verdict in ("full_pass", "partial_pass", "weak_partial", "fail")
    }
    passed_cases = quality_passed_cases - inconclusive_cases
    all_gates_passed = bool(gates) and all(
        gate.passed
        for gate in gates
        if not (
            gate.coverage
            and any(
                result.case_id == gate.case_id and result.completion_kind == "partial"
                for result in results
            )
        )
    )
    status = (
        "fail"
        if not all_gates_passed
        else "inconclusive"
        if inconclusive_cases
        else "pass"
    )
    return {
        "schema_version": 1,
        "evaluator": "web-researcher-live",
        "mode": mode,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "fixture": str(fixture_path),
        "status": status,
        "passed": status == "pass",
        "conclusive": not inconclusive_cases,
        "summary": {
            "cases": len(results),
            "passed_cases": len(passed_cases),
            "quality_passed_cases": len(quality_passed_cases),
            "full_pass_cases": verdict_counts["full_pass"],
            "partial_pass_cases": verdict_counts["partial_pass"],
            "weak_partial_cases": verdict_counts["weak_partial"],
            "failed_cases": verdict_counts["fail"],
            "infrastructure_inconclusive_cases": len(inconclusive_cases),
            "direct_cases": sum(result.mode == "direct" for result in results),
            "delegated_cases": sum(result.mode == "delegated" for result in results),
            "answer_ready_p50_ms": statistics.median(durations) if durations else None,
            "answer_ready_p95_ms": percentile(durations, 0.95),
            "longest_answer_ready_ms": max(durations) if durations else None,
            "citations": sum(len(result.citations) for result in results),
            "resolvable_citations": sum(
                probe.status is not None and 200 <= probe.status < 400 for probe in probes
            ),
            "evidence_supported_answers": sum(
                result.judge_supported is True for result in results
            ),
            "tool_calls": len(tool_calls),
            "failed_tool_calls": sum(not call.success for call in tool_calls),
            "llm_calls": len(llm_calls),
            "failed_llm_calls": sum(value_is_false(call.get("success")) for call in llm_calls),
            "input_tokens": sum(int(call.get("input_tokens") or 0) for call in llm_calls),
            "output_tokens": sum(int(call.get("output_tokens") or 0) for call in llm_calls),
            "reasoning_tokens": sum(int(call.get("reasoning_tokens") or 0) for call in llm_calls),
            "cost_usd": sum(float(call.get("cost_usd") or 0.0) for call in llm_calls),
        },
        "gates": [asdict(gate) for gate in gates],
        "cases": [
            {key: value for key, value in asdict(result).items() if key != "settled_events"}
            for result in results
        ],
    }


def write_report(
    output_dir: Path,
    mode: str,
    fixture_path: Path,
    results: list[CaseResult],
    gates: list[Gate],
) -> dict[str, Any]:
    output_dir.mkdir(parents=True, exist_ok=True)
    payload = report_payload(mode, fixture_path, results, gates)
    (output_dir / "report.json").write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    summary = payload["summary"]
    case_rows = "\n".join(
        "<tr>"
        f"<td>{escape(result.case_id)}</td>"
        f"<td>{escape(result.mode)}</td>"
        f"<td>{escape(result.root_status)}</td>"
        f"<td class='{('pass' if result.verdict != 'fail' else 'fail')}'>{escape(result.verdict)}"
        + (
            f"<br><small>{escape(result.completion_kind or '')} · open={len(result.open_items)} · {escape(result.honesty or '')}</small>"
            if result.completion_kind
            else ""
        )
        + "</td>"
        f"<td>{escape(', '.join(result.execution_agents))}</td>"
        f"<td>{result.answer_ready_ms / 1000:.1f}s</td>"
        f"<td>{len(result.citations)}</td>"
        f"<td class='{('pass' if result.judge_supported else 'fail')}'>{'PASS' if result.judge_supported else 'FAIL'}</td>"
        f"<td>{len(result.tool_calls)}</td>"
        f"<td>{len(result.llm_calls)}</td>"
        f"<td>${sum(float(call.get('cost_usd') or 0) for call in result.llm_calls):.5f}</td>"
        f"<td>{escape(result.error or '')}</td>"
        "</tr>"
        for result in results
    )
    gate_rows = "\n".join(
        "<tr>"
        f"<td>{escape(gate.case_id)}</td>"
        f"<td>{escape(gate.name)}</td>"
        f"<td class='{('pass' if gate.passed else 'fail')}'>{'PASS' if gate.passed else 'FAIL'}</td>"
        f"<td>{escape(gate.detail)}</td>"
        "</tr>"
        for gate in gates
    )
    citation_rows = "\n".join(
        "<tr>"
        f"<td>{escape(result.case_id)}</td>"
        f"<td><a href='{escape(url, quote=True)}'>{escape(url)}</a></td>"
        f"<td>{escape(hostname(url))}</td>"
        f"<td>{escape(next((str(probe.status) for probe in result.citation_probes if probe.url == url), 'not probed'))}</td>"
        f"<td>{escape(next((probe.final_url or '' for probe in result.citation_probes if probe.url == url), ''))}</td>"
        "</tr>"
        for result in results
        for url in result.citations
    ) or "<tr><td colspan='5'>No citations captured.</td></tr>"
    tool_rows = "\n".join(
        "<tr>"
        f"<td>{escape(result.case_id)}</td>"
        f"<td>{escape(call.tool_name)}</td>"
        f"<td class='{('pass' if call.success else 'fail')}'>{'PASS' if call.success else 'FAIL'}</td>"
        f"<td>{call.duration_ms if call.duration_ms is not None else ''}</td>"
        f"<td>{escape(call.error or '')}</td>"
        "</tr>"
        for result in results
        for call in result.tool_calls
    ) or "<tr><td colspan='5'>No completed tool calls captured.</td></tr>"
    llm_rows = "\n".join(
        "<tr>"
        f"<td>{escape(result.case_id)}</td>"
        f"<td>{escape(str(call.get('operation') or ''))}</td>"
        f"<td>{escape(str(call.get('profile') or ''))}</td>"
        f"<td>{escape(str(call.get('provider') or ''))}</td>"
        f"<td>{escape(str(call.get('model') or ''))}</td>"
        f"<td class='{('fail' if value_is_false(call.get('success')) else 'pass')}'>{'FAIL' if value_is_false(call.get('success')) else 'PASS'}</td>"
        f"<td>{float(call.get('latency_ms') or 0):.1f}</td>"
        f"<td>{int(call.get('input_tokens') or 0)} / {int(call.get('output_tokens') or 0)} / {int(call.get('reasoning_tokens') or 0)}</td>"
        f"<td>${float(call.get('cost_usd') or 0):.6f}</td>"
        "</tr>"
        for result in results
        for call in result.llm_calls
    ) or "<tr><td colspan='9'>No linked LLM calls captured.</td></tr>"
    phase_rows = "\n".join(
        "<tr>"
        f"<td>{escape(result.case_id)}</td>"
        f"<td>{escape(name)}</td>"
        f"<td>{duration_ms:.1f}</td>"
        "</tr>"
        for result in results
        for name, duration_ms in sorted(result.phase_timings_ms.items())
    ) or "<tr><td colspan='3'>No runtime phase timings captured.</td></tr>"
    answer_rows = "\n".join(
        "<details><summary>"
        f"{escape(result.case_id)} · {result.answer_chars} chars"
        "</summary><pre>"
        f"{escape(result.answer_excerpt)}"
        "</pre></details>"
        for result in results
    )
    p50 = summary["answer_ready_p50_ms"]
    p95 = summary["answer_ready_p95_ms"]
    html = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Web researcher live evaluation</title>
<style>
body{{font:14px/1.5 system-ui,-apple-system,sans-serif;margin:32px;background:#10131a;color:#e8edf5}}
h1,h2{{letter-spacing:-.02em}} .meta{{color:#9eabc0}} .cards{{display:flex;gap:12px;flex-wrap:wrap}}
.card{{background:#171c26;border:1px solid #2a3344;border-radius:12px;padding:12px 16px;min-width:130px}}
table{{width:100%;border-collapse:collapse;margin:12px 0 28px;background:#151a23}}
th,td{{text-align:left;vertical-align:top;border-bottom:1px solid #2a3344;padding:9px}}
th{{color:#b8c5d9}} .pass{{color:#63d392;font-weight:700}} .fail{{color:#ff7b86;font-weight:700}}
code,pre{{color:#b8d7ff}} pre{{white-space:pre-wrap;background:#151a23;padding:14px;border-radius:10px}}
a{{color:#85b8ff}} details{{margin:10px 0}}
</style></head><body>
<h1>{escape(str(payload['status']).upper())} · Web researcher live evaluation</h1>
<p class="meta">Mode: {escape(mode)} · Fixture: <code>{escape(str(fixture_path))}</code> · {escape(payload['generated_at'])}</p>
<div class="cards">
<div class="card"><strong>{summary['passed_cases']}/{summary['cases']}</strong><br>cases passed</div>
<div class="card"><strong>{(p50 or 0) / 1000:.1f}s</strong><br>answer-ready p50</div>
<div class="card"><strong>{(p95 or 0) / 1000:.1f}s</strong><br>answer-ready p95</div>
<div class="card"><strong>{(summary['longest_answer_ready_ms'] or 0) / 1000:.1f}s</strong><br>longest answer-ready</div>
<div class="card"><strong>{summary['citations']}</strong><br>citations</div>
<div class="card"><strong>{summary['tool_calls']}</strong><br>tool calls</div>
<div class="card"><strong>{summary['llm_calls']}</strong><br>LLM calls</div>
<div class="card"><strong>${summary['cost_usd']:.5f}</strong><br>observed cost</div>
</div>
<h2>Cases</h2><table><thead><tr><th>Case</th><th>Path</th><th>Status</th><th>Verdict</th><th>Execution agents</th><th>Answer ready</th><th>Citations</th><th>Evidence</th><th>Tools</th><th>LLM calls</th><th>Cost</th><th>Error</th></tr></thead><tbody>{case_rows}</tbody></table>
<h2>Gates</h2><table><thead><tr><th>Case</th><th>Gate</th><th>Status</th><th>Evidence</th></tr></thead><tbody>{gate_rows}</tbody></table>
<h2>Tool calls</h2><table><thead><tr><th>Case</th><th>Tool</th><th>Status</th><th>Duration ms</th><th>Error</th></tr></thead><tbody>{tool_rows}</tbody></table>
<h2>LLM calls</h2><table><thead><tr><th>Case</th><th>Operation</th><th>Profile</th><th>Provider</th><th>Model</th><th>Status</th><th>Latency ms</th><th>Input / output / reasoning tokens</th><th>Cost</th></tr></thead><tbody>{llm_rows}</tbody></table>
<h2>Runtime phase timings</h2><table><thead><tr><th>Case</th><th>Phase</th><th>Duration ms</th></tr></thead><tbody>{phase_rows}</tbody></table>
<h2>Citations</h2><table><thead><tr><th>Case</th><th>URL</th><th>Domain</th><th>Reachability HTTP</th><th>Final URL</th></tr></thead><tbody>{citation_rows}</tbody></table>
<h2>Answer evidence</h2>{answer_rows}
<p><a href="report.json">Raw JSON evidence</a></p>
</body></html>"""
    (output_dir / "report.html").write_text(html, encoding="utf-8")
    return payload


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--api-base-url",
        default=os.environ.get("WEB_RESEARCHER_LIVE_API_BASE_URL", DEFAULT_BASE_URL),
    )
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument(
        "--case",
        action="append",
        dest="case_ids",
        help="Run only the named fixture case (repeatable)",
    )
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--http-timeout-secs", type=float, default=120.0)
    parser.add_argument("--citation-probe-limit", type=int, default=3)
    parser.add_argument(
        "--llm-profile",
        default=os.environ.get(
            "WEB_RESEARCHER_LIVE_LLM_PROFILE", DEFAULT_EVAL_LLM_PROFILE
        ),
        help=(
            "Configured named profile used for task agentic decisions, the "
            "terminal grounding critic, and the eval-only evidence judge "
            f"(default: {DEFAULT_EVAL_LLM_PROFILE})"
        ),
    )
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    # Accepted for the uniform aggregate-runner interface. Runtime routing is
    # resolved by the running backend; the evaluator never calls a provider.
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
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    configure_ca_bundle()
    if args.runs <= 0:
        raise EvalFailure("--runs must be a positive integer")
    if args.http_timeout_secs <= 0:
        raise EvalFailure("--http-timeout-secs must be positive")
    if args.citation_probe_limit < 0:
        raise EvalFailure("--citation-probe-limit must be non-negative")
    args.llm_profile = args.llm_profile.strip()
    if not LLM_PROFILE_RE.fullmatch(args.llm_profile):
        raise EvalFailure(
            "--llm-profile must be a configured profile name using only "
            "letters, numbers, '.', '_', ':', or '-' (maximum 128 characters)"
        )
    cases = load_cases(args.fixtures)
    if args.case_ids:
        requested = set(args.case_ids)
        available = {str(case["id"]) for case in cases}
        unknown = sorted(requested - available)
        if unknown:
            raise EvalFailure(
                f"unknown --case value(s): {unknown}; available={sorted(available)}"
            )
        cases = [case for case in cases if str(case["id"]) in requested]
    if args.self_test:
        results, gates = synthetic_self_test(cases, args.llm_profile)
        mode = "self-test"
    elif args.dry_run:
        results = []
        gates = [
            *validate_agent_contract(cases),
            Gate("dry_run.fixture_valid", True, f"cases={len(cases)} fixture={args.fixtures}"),
            Gate("routing.eval_profile", True, args.llm_profile),
        ]
        mode = "dry-run"
    else:
        client = Client(
            args.api_base_url,
            args.http_timeout_secs,
        )
        results = []
        gates = validate_agent_contract(cases)
        gates.append(Gate("routing.eval_profile", True, args.llm_profile))
        runtime_contract_gates = validate_runtime_agent_contract(client)
        gates.extend(runtime_contract_gates)
        mode = "live"
        stopped = False
        if not all(gate.passed for gate in runtime_contract_gates):
            print(
                "Scoped web-researcher runtime contract is stale; refusing to spend on cases.",
                flush=True,
            )
        else:
            print("Press Ctrl-C at any time to stop the active eval task safely.", flush=True)
            for repeat in range(1, args.runs + 1):
                for case in cases:
                    print(
                        f"[{case['id']}#{repeat}] {case['mode']} via {case['root_agent_id']} "
                        "(answer-ready latency is observational)",
                        flush=True,
                    )
                    result, case_gates = run_case(
                        client,
                        case,
                        repeat,
                        args.citation_probe_limit,
                        args.llm_profile,
                        args.delete_tasks,
                    )
                    results.append(result)
                    gates.extend(case_gates)
                    if result.interrupted:
                        stopped = True
                    print(
                        f"  status={result.root_status} answer={result.answer_ready_ms / 1000:.1f}s "
                        f"citations={len(result.citations)} tools={len(result.tool_calls)} "
                        f"error={result.error or '-'}",
                        flush=True,
                    )
                    if stopped:
                        break
                if stopped:
                    break
    payload = write_report(args.output_dir, mode, args.fixtures, results, gates)
    print(f"Web-researcher eval report: {args.output_dir / 'report.html'}")
    print(
        f"Result: {str(payload['status']).upper()} "
        f"({payload['summary']['passed_cases']}/{payload['summary']['cases']} cases: "
        f"{payload['summary'].get('full_pass_cases', 0)} full / "
        f"{payload['summary'].get('partial_pass_cases', 0)} partial"
        + (
            f" ({payload['summary']['weak_partial_cases']} weak)"
            if payload['summary'].get('weak_partial_cases')
            else ""
        )
        + f" / {payload['summary'].get('failed_cases', 0)} fail)"
    )
    if any(result.interrupted for result in results):
        return 130
    if payload["status"] == "inconclusive":
        return 2
    return 0 if payload["passed"] else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except EvalFailure as error:
        print(f"web-researcher evaluator configuration error: {error}", flush=True)
        raise SystemExit(2) from error

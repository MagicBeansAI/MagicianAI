#!/usr/bin/env python3
"""Harness conformance evals: does Magician keep working when a component is
swapped for an external harness?

Five lanes share this rig (see docs/archive/plans/2026-09-11-harness-conformance-evals.md
and docs/archive/plans/2026-09-14-parent-engine-routing.md):

  chat       the chat mouth is an external CLI (`chat.harness_engine`)
  run        the run engine is an external CLI (`execution.harness_engine`):
             a real task runs under each engine, and its LLM trace must show
             the text-only operations on that engine and the tool-carrying
             decision never on a harness
  execution  the agentic loop's decide phase is an external CLI
  voice      GPT Realtime / GPT Live 1 delegate work to Magician
  plane      an external harness drives Magician over the plane MCP door

Every lane grades by EFFECT (the thing asked for exists afterwards) plus one
proof that the swapped component did the work and Magician did not silently
fall back. Verdicts per engine x case: pass | partial | fail | cli_unavailable.

The chat and run lanes are implemented here; the others are registered so the
report and the /evals page keep one shape, and each says so when invoked.

Live mode needs a running runtime (``MAGICIAN_BEARER_TOKEN`` in the
environment, token scope = the workspace under test) and the harness CLIs on
PATH and signed in. ``--self-test`` is provider-free and exercises the report,
verdict and fixture logic only.

Engine switches persist to the live config. The rig reads the current values
first and restores them in ``finally``, including on Ctrl-C; a restore failure
is printed last so it cannot be missed.
"""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import secrets
import shutil
import signal
import subprocess
import sys
import threading
import functools
import time
from dataclasses import dataclass, field, asdict
from html import escape
from pathlib import Path
from typing import Any, Iterable, Iterator
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode
from urllib.request import Request, build_opener

print = functools.partial(print, flush=True)  # noqa: A001 - background runs must not buffer progress
REPO_ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = REPO_ROOT / "scripts/fixtures/harness_conformance"
DEFAULT_BASE_URL = "http://127.0.0.1:3002"
DEFAULT_OUTPUT_DIR = REPO_ROOT / "coverage/evals/harness-conformance/live/latest"
LANES = ("chat", "run", "execution", "voice", "plane")
VERDICTS = ("pass", "partial", "fail", "cli_unavailable", "inconclusive")
MAX_RESPONSE_BYTES = 8 * 1024 * 1024
CLI_PROBE_TIMEOUT_SECONDS = 120.0
TASK_EFFECT_POLL_SECONDS = 20.0
HITL_POLL_SECONDS = 2.0
TASK_LIST_PAGE_LIMIT = 50
TASK_LIST_MAX_PAGES = 6
THREAD_LIST_PAGE_LIMIT = 100
RUN_TERMINAL_STATUSES = frozenset({"completed", "failed", "cancelled", "canceled"})
# The states a run sits in until a person acts. The chat lane answers the
# mouth's clarifications for the owner; a run has no such answerer here, so
# a run that settles into one of these has stopped, and the stop is the
# finding.
RUN_WAITING_STATUSES = frozenset(
    {"waiting_for_user", "waiting_for_input", "waiting_for_confirmation", "paused", "paused_by_user"}
)
RUN_POLL_SECONDS = 3.0
RUN_EVENTS_LIMIT = 4000
# LLM facts materialise behind the run; the run lane polls for the task's
# rows this long before it calls the trace unreadable.
ANALYTICS_POLL_SECONDS = 90.0
ANALYTICS_POLL_INTERVAL_SECONDS = 5.0
HARNESS_PROVIDER_PREFIX = "harness-"
# The decide turn and its variants are the operations that carry tools; the
# parent-engine rule never routes those to a harness provider.
TOOL_CARRYING_OPERATION_PREFIX = "agentic_decision"
# Roster engine -> the family its stateless harness provider is named after
# (`harness-<family>`). App Server rides the Codex CLI for one-shot calls.
# Claude Code keeps its `_code`: the magicllm provider is `harness-claude_code`
# (`op-harness-claude` in `llm-router.yaml` declares
# `provider: harness-claude_code`) — it is the PROFILE name that drops the
# suffix, not the provider. `magician` is never a parent.
ENGINE_PROVIDER_FAMILY = {
    "claude_code": "claude_code",
    "codex": "codex",
    "codex_app_server": "codex",
    "grok": "grok",
    "agy": "agy",
}

# How each roster engine is launched for a headless liveness probe. This is
# the same CLI the plane engine spawns; a probe failure here means "the CLI is
# not usable on this machine", which the lane must report as cli_unavailable
# rather than as a Magician defect. Nested Claude Code refuses to start with
# its own session markers in the environment, so those are dropped.
CLI_PROBES: dict[str, dict[str, Any]] = {
    "claude_code": {
        "binary": "claude",
        "argv": ["claude", "-p", "Reply with exactly: PONG", "--output-format", "json", "--max-turns", "1"],
        "unset_env": ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"],
    },
    "codex": {
        "binary": "codex",
        "argv": ["codex", "exec", "--json", "--skip-git-repo-check", "Reply with exactly: PONG"],
        "unset_env": [],
    },
    "codex_app_server": {
        "binary": "codex",
        "argv": ["codex", "exec", "--json", "--skip-git-repo-check", "Reply with exactly: PONG"],
        "unset_env": [],
    },
    "grok": {"binary": "grok", "argv": ["grok", "-p", "Reply with exactly: PONG"], "unset_env": []},
    "agy": {"binary": "agy", "argv": ["agy", "-p", "Reply with exactly: PONG"], "unset_env": []},
}


# Every name the chat-engine roster can carry: the built-in mouth plus the
# probed harness CLIs. Fixture options that name engines are checked here.
KNOWN_ENGINES = ("magician", *CLI_PROBES)


class EvalFailure(RuntimeError):
    pass


class RuntimeUnavailable(EvalFailure):
    """The runtime went away mid-case (restart by another operator). Cases that
    hit this are inconclusive, never failures."""


class PreconditionFailure(EvalFailure):
    """A step the case itself depends on (not the mouth) did not hold: a probe
    task the oracle needs gone is still there. The case is a `fail` naming the
    precondition, so a broken oracle can never pass by accident."""


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------


class Client:
    def __init__(self, base_url: str, timeout: float):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.opener = build_opener()

    def _headers(self, accept: str) -> dict[str, str]:
        headers = {"Accept": accept, "Content-Type": "application/json"}
        token = os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
        if token:
            headers["Authorization"] = f"Bearer {token}"
        return headers

    def _url(self, path: str, query: dict[str, Any] | None) -> str:
        params = {k: v for k, v in (query or {}).items() if v is not None}
        if not params:
            return f"{self.base_url}{path}"
        sep = "&" if "?" in path else "?"
        return f"{self.base_url}{path}{sep}{urlencode(params)}"

    def request(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        timeout: float | None = None,
    ) -> tuple[int, bytes, float]:
        data = json.dumps(body).encode("utf-8") if body is not None else None
        request = Request(self._url(path, query), data=data, method=method, headers=self._headers("application/json"))
        started = time.perf_counter()
        try:
            response = self.opener.open(request, timeout=timeout or self.timeout)
            status = response.status
            raw = response.read(MAX_RESPONSE_BYTES + 1)
        except HTTPError as error:
            status = error.code
            raw = error.read(MAX_RESPONSE_BYTES + 1)
        except (URLError, TimeoutError, OSError) as error:
            raise RuntimeUnavailable(f"{method} {path}: {error}") from error
        latency_ms = (time.perf_counter() - started) * 1000
        if len(raw) > MAX_RESPONSE_BYTES:
            raise EvalFailure(f"response exceeded {MAX_RESPONSE_BYTES} bytes: {path}")
        return status, raw, latency_ms

    def json(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        expected: Iterable[int] = (200,),
        timeout: float | None = None,
    ) -> tuple[Any, float]:
        status, raw, latency = self.request(method, path, body, query, timeout=timeout)
        try:
            payload = json.loads(raw) if raw else None
        except json.JSONDecodeError as error:
            raise EvalFailure(f"invalid JSON from {method} {path} (HTTP {status})") from error
        if status not in set(expected):
            raise EvalFailure(f"{method} {path} -> HTTP {status}: {json.dumps(payload)[:400]}")
        return payload, latency

    def sse(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        timeout: float | None = None,
    ) -> tuple[list[SseFrame], float]:
        """Send one request and read its `text/event-stream` body to the end,
        frame by frame. The timeout bounds every wait on the socket, so a
        stalled stream surfaces the same way a stalled response does; the
        latency covers the whole stream, since the answer is only complete at
        its last frame. A non-200 status is an error body, never a stream."""
        data = json.dumps(body).encode("utf-8") if body is not None else None
        request = Request(self._url(path, None), data=data, method=method, headers=self._headers("text/event-stream"))
        started = time.perf_counter()
        try:
            with contextlib.closing(self.opener.open(request, timeout=timeout or self.timeout)) as response:
                frames = list(parse_sse(bounded_lines(response, path)))
        except HTTPError as error:
            raw = error.read(MAX_RESPONSE_BYTES + 1)
            raise EvalFailure(f"{method} {path} -> HTTP {error.code}: {raw[:400]!r}") from error
        except (URLError, TimeoutError, OSError) as error:
            raise RuntimeUnavailable(f"{method} {path}: {error}") from error
        return frames, (time.perf_counter() - started) * 1000


def bounded_lines(response: Any, path: str) -> Iterator[bytes]:
    """The response body line by line, refusing past the same byte bound the
    buffered reads honour. Each read is itself capped, so a line that never
    terminates cannot grow past the bound before it is counted."""
    total = 0
    while True:
        line = response.readline(MAX_RESPONSE_BYTES + 1)
        if not line:
            return
        total += len(line)
        if total > MAX_RESPONSE_BYTES:
            raise EvalFailure(f"response exceeded {MAX_RESPONSE_BYTES} bytes: {path}")
        yield line


@dataclass
class SseFrame:
    event: str
    data: str


def parse_sse(lines: Iterable[bytes]) -> Iterator[SseFrame]:
    """Server-sent events, as the stream endpoint writes them: an `event:`
    line naming the frame, one or more `data:` lines, and a blank line that
    dispatches the frame. Comment lines and the fields this rig has no use
    for are skipped; a stream that ends mid-frame still yields that frame."""
    event = ""
    data: list[str] = []
    for raw in lines:
        line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
        if not line:
            if event or data:
                yield SseFrame(event or "message", "\n".join(data))
            event, data = "", []
            continue
        if line.startswith(":"):
            continue
        name, _, value = line.partition(":")
        value = value[1:] if value.startswith(" ") else value
        if name == "event":
            event = value
        elif name == "data":
            data.append(value)
    if event or data:
        yield SseFrame(event or "message", "\n".join(data))


# ---------------------------------------------------------------------------
# Results
# ---------------------------------------------------------------------------


@dataclass
class TurnRecord:
    turn_index: int
    chat_turn_id: str
    prompt: str
    answer: str
    provider: str | None
    model: str | None
    cost_usd: float
    input_tokens: int
    output_tokens: int
    latency_ms: float
    harness_event_seen: bool
    tool_calls: list[str] = field(default_factory=list)
    # `event: token` frames read when the turn was streamed; 0 for a turn
    # sent to the synchronous route.
    token_events: int = 0


@dataclass
class CaseResult:
    lane: str
    engine: str
    case_id: str
    run: int
    verdict: str
    reason: str
    effect_ok: bool | None
    proof_ok: bool | None
    effect_detail: str
    turns: list[TurnRecord] = field(default_factory=list)
    artifacts: dict[str, Any] = field(default_factory=dict)
    total_latency_ms: float = 0.0
    # None when the fixture names no `no_tools_turn` (or that turn never ran);
    # False when hands were used on the turn the case forbids them.
    no_tools_ok: bool | None = None
    # None when the fixture sets no `min_token_events` (or no turn ran);
    # False when a streamed turn carried fewer token frames than the bound.
    stream_ok: bool | None = None
    # None when the fixture sets no `turn_answer_must_not_contain` (or that
    # turn never ran); False when that turn's answer carried the withheld value.
    no_leak_ok: bool | None = None
    # Run lane, `trace: true` cases only (None otherwise, or when no analytics
    # row was read). False when a tool-carrying operation rode a harness
    # provider, or any operation did on the `magician` baseline.
    trace_ok: bool | None = None
    # Run lane, `trace: true` cases on an external engine only; False when no
    # text-only operation rode the parent's harness provider.
    trace_followed: bool | None = None


@dataclass
class EngineSummary:
    engine: str
    installed: bool
    cli_probe: str
    verdict_counts: dict[str, int]
    total_latency_ms: float


# The trace reasons, as constants: the run grader appends the operations it
# saw only when one of these is the reason the rule settled on.
TRACE_TOOL_CALL_RODE_HARNESS = "tool_call_rode_harness: a tool-carrying operation rode a harness provider"
TRACE_BASELINE_RODE_HARNESS = "baseline_op_rode_harness: an operation of the magician baseline rode a harness provider"
TRACE_NO_ANALYTICS_ROWS = "no_analytics_rows: no llm_calls row was attributed to the task"
TRACE_NO_TEXT_OP_ON_PARENT = "no_text_op_on_parent: no text-only operation rode the parent engine"
TRACE_NO_TEXT_OP_OBSERVED = "no_text_op_observed: only tool-carrying operations were attributed to the task"


def decide_verdict(
    *,
    effect_ok: bool | None,
    proof_ok: bool | None,
    baseline: bool,
    partial_on_effect_miss: bool,
    cli_available: bool,
    inconclusive: bool,
    tools_expected: bool = False,
    hands_attributed: bool | None = None,
    no_tools_ok: bool | None = None,
    stream_ok: bool | None = None,
    stream_exempt: bool = False,
    no_leak_ok: bool | None = None,
    trace_rows: int | None = None,
    trace_ok: bool | None = None,
    trace_followed: bool | None = None,
) -> tuple[str, str]:
    """The one place the verdict rule lives. Effect over method: no effect is
    a failure (or a declared partial); a present effect with the wrong author
    is a failure too, because "the harness answered" is the whole claim.

    An effect that needed a tool is a `pass` only when the tool-lineage facts
    attribute the hands to this turn. A harness can reach the same effect by
    other means (a native shell, a credential read off disk), and the plane
    is the only sanctioned route; an unattributed effect is therefore
    `partial`, never `pass`.

    A turn the fixture declares tool-free (`no_tools_turn`) is an oracle on
    memory: the effect there must come from what the mouth already knows.
    Hands on that turn fail the case even when the effect is present, since
    the effect then proves nothing about recall.

    A turn the fixture tells to withhold a value (`turn_answer_must_not_contain`)
    guards the same oracle from the other side: a later recall of a value that
    an earlier answer already spelled out can be a transcript replay, so a leak
    fails the case whatever the recall turn says.

    A streamed turn (`stream` with `min_token_events`) is an oracle on the
    mouth's streaming: the answer must arrive as at least that many token
    frames, or the mouth only pretends to stream. Too few frames fail the
    case; an engine the fixture exempts (`engines_exempt`) grades `partial`
    instead, because the shortfall is documented rather than a regression.

    A run whose fixture grades the trace (`trace: true`) reads the `llm_calls`
    facts attributed to its task; `trace_rows` is how many were read (None
    when the case grades no trace), and none read is `inconclusive`, because
    the rule cannot be measured without them. `trace_ok` is the floor of the
    parent-engine rule: no tool-carrying operation rode a harness provider,
    and on the `magician` baseline no operation did, since `magician` is never
    a parent; False fails the case. `trace_followed` is the rule itself: at
    least one text-only operation rode the parent's own harness provider;
    False is `partial`, because the run did its work on the right engine and
    only its background operations stayed behind.

    Order: the no-tools and no-leak checks come before the effect check
    because they decide whether the effect can count as evidence at all; the
    stream check comes after the effect and proof checks because it only
    qualifies how a genuine answer arrived, and a missing effect or a wrong
    author is the larger finding and must be the reason. The trace checks
    come after the stream check for the same reason: they qualify what the
    run's background did, once the run itself is known to have done its work
    on the right engine."""
    if inconclusive:
        return "inconclusive", "runtime unavailable mid-case"
    if not cli_available:
        return "cli_unavailable", "harness CLI not usable on this machine"
    if no_tools_ok is False:
        return "fail", "tools used on the no-tools turn"
    if no_leak_ok is False:
        return "fail", "the withheld value leaked into an earlier answer"
    if effect_ok is False:
        if partial_on_effect_miss:
            return "partial", "effect missing; fixture marks this as a deferrable effect"
        return "fail", "effect missing"
    if not baseline and proof_ok is False:
        return "fail", "fallback: Magician's own mouth answered, not the harness"
    if baseline and proof_ok is False:
        return "fail", "baseline turn was answered by a harness instead of Magician"
    if stream_ok is False:
        if stream_exempt:
            return "partial", "engine cannot stream (documented)"
        return "fail", "too few streamed token frames"
    if trace_ok is False:
        if baseline:
            return "fail", TRACE_BASELINE_RODE_HARNESS
        return "fail", TRACE_TOOL_CALL_RODE_HARNESS
    if trace_rows == 0:
        return "inconclusive", TRACE_NO_ANALYTICS_ROWS
    if effect_ok is None or proof_ok is None:
        return "partial", "effect present but a proof could not be read"
    if tools_expected and hands_attributed is not True:
        return "partial", "effect present but no tool-lineage fact attributes the hands to this turn"
    if trace_followed is False:
        return "partial", TRACE_NO_TEXT_OP_ON_PARENT
    return "pass", "effect present and the expected author did the work"


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def load_cases(path: Path, lane: str) -> list[dict[str, Any]]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvalFailure(f"cannot read fixtures {path}: {error}") from error
    if payload.get("lane") != lane:
        raise EvalFailure(f"fixture {path} is for lane {payload.get('lane')!r}, not {lane!r}")
    cases = payload.get("cases")
    if not isinstance(cases, list) or not cases:
        raise EvalFailure(f"fixture {path} has no cases")
    ids = [c.get("id") for c in cases]
    if len(set(ids)) != len(ids) or any(not isinstance(i, str) or not i for i in ids):
        raise EvalFailure(f"fixture {path} case ids must be unique non-empty strings")
    for case in cases:
        if lane == "run":
            validate_run_case(case)
        else:
            validate_chat_case(case)
    return cases


# The keys a run case may carry. A chat option in a run case would be
# silently ignored by the run lane, so it is a fixture error instead.
RUN_CASE_KEYS = frozenset({"id", "description", "effect", "trace", "notes"})


def validate_run_case(case: dict[str, Any]) -> None:
    """A run case is a task description the run engine works on and the
    effect the run must leave on its own task; `trace: true` adds the
    parent-engine grading of the run's LLM facts."""
    unknown = sorted(set(case) - RUN_CASE_KEYS)
    if unknown:
        raise EvalFailure(f"case {case.get('id')} has keys the run lane does not read: {unknown}")
    description = case.get("description")
    if not isinstance(description, str) or not description.strip():
        raise EvalFailure(f"case {case.get('id')} needs a non-empty task description")
    effect = case.get("effect") or {}
    if effect.get("kind") != "task_field_contains":
        raise EvalFailure(f"case {case.get('id')}: the run lane grades task_field_contains on the run's own task, not {effect.get('kind')!r}")
    for key in ("field", "value"):
        if not isinstance(effect.get(key), str) or not effect[key].strip():
            raise EvalFailure(f"case {case.get('id')}: task_field_contains needs a non-empty {key}")
    trace = case.get("trace")
    if trace is not None and not isinstance(trace, bool):
        raise EvalFailure(f"case {case.get('id')}: trace must be a boolean")


def validate_chat_case(case: dict[str, Any]) -> None:
    turns = case.get("turns")
    if not isinstance(turns, list) or not turns or not all(isinstance(t, str) for t in turns):
        raise EvalFailure(f"case {case.get('id')} needs a non-empty list of turn prompts")
    effect = case.get("effect") or {}
    if effect.get("kind") not in {"answer_contains", "task_exists", "thread_exists", "task_field_contains"}:
        raise EvalFailure(f"case {case.get('id')} has an unknown effect kind {effect.get('kind')!r}")
    if effect.get("kind") in {"task_exists", "thread_exists"}:
        title = effect.get("title")
        if not isinstance(title, str) or not title.strip():
            raise EvalFailure(f"case {case.get('id')}: {effect['kind']} needs the title to look for")
    setup = case.get("setup")
    if setup is not None and setup.get("kind") != "create_task":
        raise EvalFailure(f"case {case.get('id')} has an unknown setup kind {setup.get('kind')!r}")
    if effect.get("kind") == "task_field_contains" and (setup is None or not effect.get("field")):
        raise EvalFailure(f"case {case.get('id')}: task_field_contains needs a create_task setup and a field")
    for option in ("delete_task_after_turn", "no_tools_turn"):
        turn_no = case.get(option)
        if turn_no is None:
            continue
        if isinstance(turn_no, bool) or not isinstance(turn_no, int) or not 1 <= turn_no <= len(turns):
            raise EvalFailure(f"case {case.get('id')}: {option} must name a turn between 1 and {len(turns)}")
    if case.get("delete_task_after_turn") is not None and setup is None:
        raise EvalFailure(f"case {case.get('id')}: delete_task_after_turn needs a create_task setup")
    withhold = case.get("turn_answer_must_not_contain")
    if withhold is not None:
        turn_no = withhold.get("turn") if isinstance(withhold, dict) else None
        value = withhold.get("value") if isinstance(withhold, dict) else None
        if isinstance(turn_no, bool) or not isinstance(turn_no, int) or not 1 <= turn_no <= len(turns):
            raise EvalFailure(f"case {case.get('id')}: turn_answer_must_not_contain.turn must name a turn between 1 and {len(turns)}")
        if not isinstance(value, str) or not value:
            raise EvalFailure(f"case {case.get('id')}: turn_answer_must_not_contain.value must be a non-empty string")
    stream = case.get("stream")
    if stream is not None and not isinstance(stream, bool):
        raise EvalFailure(f"case {case.get('id')}: stream must be a boolean")
    min_tokens = case.get("min_token_events")
    if min_tokens is not None:
        if isinstance(min_tokens, bool) or not isinstance(min_tokens, int) or min_tokens < 1:
            raise EvalFailure(f"case {case.get('id')}: min_token_events must be a positive integer")
        if stream is not True:
            raise EvalFailure(f"case {case.get('id')}: min_token_events needs stream: true")
    exempt = case.get("engines_exempt")
    if exempt is not None:
        if not isinstance(exempt, list) or not all(isinstance(e, str) and e in KNOWN_ENGINES for e in exempt):
            raise EvalFailure(f"case {case.get('id')}: engines_exempt must list engines from {list(KNOWN_ENGINES)}")
        if min_tokens is None:
            raise EvalFailure(f"case {case.get('id')}: engines_exempt needs min_token_events")


def no_tools_check(case: dict[str, Any], turns: list[TurnRecord]) -> tuple[bool | None, list[str]]:
    """Whether the fixture's `no_tools_turn` ran without hands, and the names
    it used when it did not. None when the case names no such turn or that
    turn never ran. Shared by the live run and `regrade`, which may learn of
    more calls from the tool-lineage facts than the turn's events carried."""
    wanted = case.get("no_tools_turn")
    if wanted is None:
        return None, []
    for turn in turns:
        if turn.turn_index == int(wanted):
            return not turn.tool_calls, list(turn.tool_calls)
    return None, []


def stream_check(case: dict[str, Any], turns: list[TurnRecord]) -> tuple[bool | None, str]:
    """Whether every streamed turn carried at least the fixture's
    `min_token_events` token frames, and a description of the first turn that
    did not. None when the case sets no bound or no turn ran. Shared by the
    live run and `regrade`, which reads the counts back off the report."""
    wanted = case.get("min_token_events")
    if wanted is None or not case.get("stream") or not turns:
        return None, ""
    for turn in turns:
        if turn.token_events < int(wanted):
            return False, f"turn {turn.turn_index} streamed {turn.token_events} token frame(s), wanted at least {wanted}"
    return True, ""


def stream_exempt(case: dict[str, Any], engine: str) -> bool:
    return engine in (case.get("engines_exempt") or [])


def leak_check(case: dict[str, Any], result: CaseResult) -> tuple[bool | None, str]:
    """Whether the turn the fixture's `turn_answer_must_not_contain` names kept
    the withheld value out of its answer, and how it did not. The value is
    rendered when the case runs and kept on the result (`withheld_value`), so
    `regrade` checks the same string. None when the case sets no such turn,
    that turn never ran, or the rendered value is not on the result."""
    option = case.get("turn_answer_must_not_contain")
    withheld = result.artifacts.get("withheld_value")
    if not isinstance(option, dict) or not isinstance(withheld, str) or not withheld:
        return None, ""
    for turn in result.turns:
        if turn.turn_index == int(option["turn"]):
            if withheld.lower() in turn.answer.lower():
                return False, f"turn {turn.turn_index} answered with {withheld!r}"
            return True, ""
    return None, ""


def grade_case_result(case: dict[str, Any], engine: str, baseline: bool, result: CaseResult) -> None:
    """The one grader: from a result whose effect and proof are measured,
    derive the hands, the three oracle checks, the verdict and its reason.
    The live run and `regrade` both come through here, so a report regraded
    under a newer rule reads exactly as a fresh run would."""
    result.artifacts["hands"] = sorted({n for t in result.turns for n in t.tool_calls})
    result.no_tools_ok, forbidden_hands = no_tools_check(case, result.turns)
    result.stream_ok, stream_detail = stream_check(case, result.turns)
    result.no_leak_ok, leak_detail = leak_check(case, result)
    result.verdict, result.reason = decide_verdict(
        effect_ok=result.effect_ok,
        proof_ok=result.proof_ok,
        baseline=baseline,
        partial_on_effect_miss=bool(case.get("partial_on_effect_miss")),
        cli_available=True,
        inconclusive=False,
        tools_expected=bool(case.get("tools_expected")),
        hands_attributed=bool(result.artifacts["hands"]),
        no_tools_ok=result.no_tools_ok,
        stream_ok=result.stream_ok,
        stream_exempt=stream_exempt(case, engine),
        no_leak_ok=result.no_leak_ok,
    )
    if result.no_tools_ok is False:
        result.reason = f"{result.reason}: {', '.join(forbidden_hands)}"
    if result.no_leak_ok is False:
        result.reason = f"{result.reason}: {leak_detail}"
    if result.stream_ok is False:
        result.reason = f"{result.reason}: {stream_detail}"
    if result.verdict == "partial" and case.get("partial_reason") and result.effect_ok is False:
        result.reason = f"{result.reason}: {case['partial_reason']}"


def render(template: str, values: dict[str, str]) -> str:
    for key, value in values.items():
        template = template.replace("{" + key + "}", value)
    return template


# ---------------------------------------------------------------------------
# CLI probes
# ---------------------------------------------------------------------------


def probe_cli(engine: str) -> tuple[bool, str]:
    spec = CLI_PROBES.get(engine)
    if spec is None:
        return True, "no CLI (built-in)"
    if shutil.which(spec["binary"]) is None:
        return False, f"{spec['binary']} not on PATH"
    env = {k: v for k, v in os.environ.items() if k not in set(spec["unset_env"])}
    try:
        completed = subprocess.run(
            spec["argv"],
            env=env,
            cwd=str(REPO_ROOT),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=CLI_PROBE_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return False, f"{spec['binary']} did not answer within {CLI_PROBE_TIMEOUT_SECONDS:.0f}s"
    except OSError as error:
        return False, f"{spec['binary']} could not start: {error}"
    text = (completed.stdout or "") + (completed.stderr or "")
    if "PONG" in text:
        return True, "answered"
    tail = text.strip().splitlines()[-1] if text.strip() else f"exit {completed.returncode}"
    return False, f"no PONG in output: {tail[:200]}"


# ---------------------------------------------------------------------------
# Chat lane
# ---------------------------------------------------------------------------


def message_text(message: dict[str, Any] | None) -> str:
    if not isinstance(message, dict):
        return ""
    content = message.get("content")
    if isinstance(content, dict):
        text = content.get("text")
        if isinstance(text, str):
            return text
        return json.dumps(content)
    return str(content or "")


@dataclass
class StreamedTurn:
    answer: str
    token_events: int
    done: dict[str, Any]
    errors: list[str]
    latency_ms: float = 0.0

    def failed(self) -> bool:
        """An `error` frame with no assistant message on `done` afterwards is
        the turn failing, graded the way a failed synchronous turn is."""
        return bool(self.errors) and not message_text(self.done.get("assistant_message"))


def fold_stream(frames: Iterable[SseFrame]) -> StreamedTurn:
    """Read a streamed chat turn: the answer text, the number of `token`
    frames, the `done` payload and any `error` frames. The `done` frame
    carries the same response the synchronous route returns, so the answer is
    read from its assistant message; when that is absent (the turn failed, or
    the response slot was never filled) the concatenated tokens stand in."""
    tokens: list[str] = []
    token_events = 0
    done: dict[str, Any] = {}
    errors: list[str] = []
    for frame in frames:
        try:
            data = json.loads(frame.data) if frame.data.strip() else {}
        except json.JSONDecodeError as error:
            raise EvalFailure(f"invalid JSON in the SSE {frame.event!r} frame") from error
        if frame.event == "token":
            token_events += 1
            text = data.get("text") if isinstance(data, dict) else None
            if isinstance(text, str):
                tokens.append(text)
        elif frame.event == "done" and isinstance(data, dict):
            done = data
        elif frame.event == "error":
            errors.append(str(data.get("error") if isinstance(data, dict) else data))
    answer = message_text(done.get("assistant_message")) or "".join(tokens)
    return StreamedTurn(answer, token_events, done, errors)


def stream_turn(client: Client, session_id: str, prompt: str, chat_turn_id: str, timeout: float) -> StreamedTurn:
    """One chat turn over the streaming route, with the latency to the last
    frame. Error frames ride back on the record for the caller to record and
    grade."""
    frames, latency = client.sse(
        "POST",
        f"/api/magician/v2/chat/sessions/{quote(session_id)}/messages/stream",
        body={"text": prompt, "chat_turn_id": chat_turn_id},
        timeout=timeout,
    )
    turn = fold_stream(frames)
    turn.latency_ms = latency
    return turn


def turn_events(client: Client, session_id: str, chat_turn_id: str) -> list[dict[str, Any]]:
    try:
        payload, _ = client.json(
            "GET",
            f"/api/magician/v2/chat/sessions/{quote(session_id)}/turns/{quote(chat_turn_id)}/events",
        )
    except EvalFailure:
        return []
    if isinstance(payload, list):
        return [e for e in payload if isinstance(e, dict)]
    if isinstance(payload, dict):
        events = payload.get("events")
        return [e for e in events if isinstance(e, dict)] if isinstance(events, list) else []
    return []


def inner_event(event: dict[str, Any]) -> tuple[str, dict[str, Any]]:
    """Read the agent event out of one per-turn event row.

    The per-turn endpoint serves the sink's persisted rows verbatim: a tagged
    envelope whose `event_type` names the transport variant and whose agent
    event sits under `data.event`, carrying its own `event_type` and `payload`.
    A top-level `event` wrapper and a flat `{event_type, payload}` row are
    still read for older projections and the self-test."""
    inner: dict[str, Any] | None = None
    data = event.get("data")
    if isinstance(data, dict) and isinstance(data.get("event"), dict):
        inner = data["event"]
    elif isinstance(event.get("event"), dict):
        inner = event["event"]
    source = inner if inner is not None else event
    kind = source.get("event_type") if inner is not None else (event.get("event_type") or event.get("type"))
    payload = source.get("payload")
    return str(kind or ""), payload if isinstance(payload, dict) else {}


def harness_proof_from_events(events: list[dict[str, Any]]) -> tuple[bool, list[str]]:
    seen = False
    tool_calls: list[str] = []
    for event in events:
        kind, payload = inner_event(event)
        if kind == "llm.succeeded" and payload.get("provider") == "harness":
            seen = True
        if kind in {"tool.call.started", "tool.call.finished"}:
            name = payload.get("tool_name") or payload.get("tool") or payload.get("name")
            if isinstance(name, str) and name and name not in tool_calls:
                tool_calls.append(name)
    return seen, tool_calls


def turn_tool_facts(client: Client, chat_turn_id: str, started_ms: int) -> list[str]:
    """Distinct tool names the canonical `llm_tool_calls` lineage attributes to
    this chat turn. Magician's own mouth writes a full lifecycle per call;
    plane `tools/call` from a harness turn writes none today, which is itself
    a finding the report carries."""
    safe = "".join(ch for ch in chat_turn_id if ch.isalnum() or ch in "_-.:")
    sql = (
        "SELECT DISTINCT tool_name FROM llm_tool_calls "
        f"WHERE chat_turn_id = '{safe}' AND tool_lineage_stage = 'execution_finished'"
    )
    try:
        payload, _ = client.json(
            "POST",
            "/api/magician/v2/analytics/llm/facts/query",
            body={"sql": sql, "from_ms": started_ms - 5000, "to_ms": int(time.time() * 1000) + 60000, "limit": 200},
            expected=(200,),
            timeout=90.0,
        )
    except EvalFailure:
        return []
    rows = (payload.get("data") or {}).get("rows") if isinstance(payload, dict) else None
    return sorted({str(r.get("tool_name")) for r in rows or [] if isinstance(r, dict) and r.get("tool_name")})


def find_task_by_title(client: Client, title: str) -> dict[str, Any] | None:
    cursor: str | None = None
    for _ in range(TASK_LIST_MAX_PAGES):
        query: dict[str, Any] = {"limit": TASK_LIST_PAGE_LIMIT}
        if cursor:
            query["cursor"] = cursor
        payload, _ = client.json("GET", "/api/magician/v3/tasks", query=query)
        tasks = payload.get("tasks") if isinstance(payload, dict) else None
        for task in tasks or []:
            if isinstance(task, dict) and task.get("title") == title:
                return task
        pagination = payload.get("pagination") if isinstance(payload, dict) else None
        cursor = pagination.get("next_cursor") if isinstance(pagination, dict) else None
        if not cursor or not (pagination or {}).get("has_more"):
            return None
    return None


def wait_for_task(client: Client, title: str, deadline_seconds: float) -> dict[str, Any] | None:
    deadline = time.monotonic() + deadline_seconds
    while True:
        task = find_task_by_title(client, title)
        if task is not None or time.monotonic() >= deadline:
            return task
        time.sleep(1.0)


def thread_slug(title: str) -> str:
    """The id the UI-thread route derives from a name it is given no id for:
    lowercase, every run of non-alphanumerics folded to one hyphen."""
    folded = "".join(ch if ch.isascii() and ch.isalnum() else "-" for ch in title.strip().lower())
    return "-".join(segment for segment in folded.split("-") if segment)


def thread_matches_title(thread: dict[str, Any], title: str) -> bool:
    """Whether a UI-thread row is the thread the case asked for. The mouth's
    `create_chat_thread` takes a lowercase id and an optional display name, so
    the row matches on its name (case-insensitive) or on the id the title slugs
    to: a mouth that set only the id still created the thread it was asked for."""
    name = str(thread.get("name") or "").strip().lower()
    return name == title.strip().lower() or str(thread.get("id") or "") == thread_slug(title)


def find_thread_by_title(client: Client, title: str) -> dict[str, Any] | None:
    """The UI-thread row titled `title`, or None. The route's `q` filter keeps
    the rows whose lowercased name or id contains the query, which the slug of
    the title always does, so one bounded page holds every candidate."""
    payload, _ = client.json(
        "GET", "/api/magician/v2/ui-threads", query={"q": title, "limit": THREAD_LIST_PAGE_LIMIT}
    )
    threads = payload.get("threads") if isinstance(payload, dict) else None
    for thread in threads or []:
        if isinstance(thread, dict) and thread_matches_title(thread, title):
            return thread
    return None


def wait_for_thread(client: Client, title: str, deadline_seconds: float) -> dict[str, Any] | None:
    deadline = time.monotonic() + deadline_seconds
    while True:
        thread = find_thread_by_title(client, title)
        if thread is not None or time.monotonic() >= deadline:
            return thread
        time.sleep(1.0)


def delete_thread(client: Client, thread_id: str) -> None:
    """Delete a probe thread. The route also drops the chat sessions filed
    under it; the case's own session is filed under the rig's per-run thread,
    never the probe. Raises when the route refuses."""
    client.json("DELETE", f"/api/magician/v2/ui-threads/{quote(thread_id)}", expected=(200, 204, 404))


def create_probe_task(client: Client, title: str, description: str) -> str:
    payload, _ = client.json(
        "POST",
        "/api/magician/v3/tasks",
        body={
            "title": title,
            "description": description,
            "agent_id": "personal-assistant",
            "approved": True,
            "created_by": "user",
        },
        expected=(200, 201),
    )
    task = payload.get("task") if isinstance(payload, dict) else None
    manifest = task.get("manifest") if isinstance(task, dict) else None
    task_id = manifest.get("task_id") if isinstance(manifest, dict) else None
    if not isinstance(task_id, str) or not task_id.strip():
        raise EvalFailure(f"task create response has no task id: {json.dumps(payload)[:300]}")
    return task_id.strip()


def read_task_field(client: Client, task_id: str, field_name: str) -> str:
    payload, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}")
    task = payload.get("task") if isinstance(payload, dict) else None
    manifest = task.get("manifest") if isinstance(task, dict) else None
    state = task.get("state") if isinstance(task, dict) else None
    for holder in (manifest, state, task):
        if isinstance(holder, dict) and isinstance(holder.get(field_name), str):
            return holder[field_name]
    return ""


def wait_for_task_field(client: Client, task_id: str, field_name: str, needle: str, deadline_seconds: float) -> str:
    deadline = time.monotonic() + deadline_seconds
    while True:
        value = read_task_field(client, task_id, field_name)
        if needle.lower() in value.lower() or time.monotonic() >= deadline:
            return value
        time.sleep(1.0)


def task_readable(client: Client, task_id: str) -> bool:
    status, _, _ = client.request("GET", f"/api/magician/v3/tasks/{quote(task_id)}")
    return status == 200


def delete_task(client: Client, task_id: str) -> None:
    """Delete a probe task, files included. Raises when the route refuses;
    the caller decides whether that is a lost cleanup or a broken oracle."""
    # Without `remove_files` the route answers ok:true, files_removed:false
    # and the task stays listed and readable; only the file removal is a
    # deletion a caller can observe.
    client.json(
        "DELETE",
        f"/api/magician/v3/tasks/{quote(task_id)}",
        query={"remove_files": "true"},
        expected=(200, 202, 204, 404),
    )


def delete_task_for_oracle(client: Client, result: CaseResult, task_id: str, turn_index: int) -> None:
    """The mid-run delete behind `delete_task_after_turn`: the task must be
    gone before the next turn, or a later answer could come from a re-lookup
    and pass as recall. A refused delete, or a task that still reads after
    one, is a failed precondition, never a lost cleanup."""
    try:
        delete_task(client, task_id)
    except RuntimeUnavailable:
        raise
    except EvalFailure as error:
        print(f"  precondition: task {task_id} not deleted after turn {turn_index}: {error}", file=sys.stderr)
        raise PreconditionFailure(f"could not delete the probe task after turn {turn_index}: {error}") from error
    result.artifacts["task_deleted_after_turn"] = turn_index
    still_readable = task_readable(client, task_id)
    result.artifacts["task_readable_after_delete"] = still_readable
    if still_readable:
        print(f"  precondition: task {task_id} still readable after delete", file=sys.stderr)
        raise PreconditionFailure("probe task still readable after delete")


class HitlAnswerer:
    """While a synchronous turn is in flight, answer the clarifications the
    mouth raises for THIS session (`GET /user-requests`, `execution_id` == the
    chat session id) the way the owner would, from the fixture's `hitl_answer`
    or by restating the prompt. Questions answered are recorded on the turn.
    Without this a `need_user_input` blocks the POST until the turn timeout,
    which is a governance behaviour, not a swap defect."""

    def __init__(self, client: Client, session_id: str, answer_text: str):
        self.client = client
        self.session_id = session_id
        self.answer_text = answer_text
        self.answered: list[dict[str, str]] = []
        self._stop = threading.Event()
        self._seen: set[str] = set()
        self._thread = threading.Thread(target=self._run, name="hitl-answerer", daemon=True)

    def __enter__(self) -> "HitlAnswerer":
        self._thread.start()
        return self

    def __exit__(self, *_: Any) -> None:
        self._stop.set()
        self._thread.join(timeout=HITL_POLL_SECONDS * 2)

    def _run(self) -> None:
        while not self._stop.wait(HITL_POLL_SECONDS):
            try:
                payload, _ = self.client.json("GET", "/api/magician/v2/user-requests")
            except EvalFailure:
                continue
            requests = payload if isinstance(payload, list) else (payload or {}).get("requests") or []
            for request in requests:
                if not isinstance(request, dict):
                    continue
                request_id = str(request.get("id") or "")
                if not request_id or request_id in self._seen:
                    continue
                if str(request.get("execution_id") or "") != self.session_id:
                    continue
                self._seen.add(request_id)
                question = str(request.get("question") or request.get("prompt") or "")
                try:
                    self.client.json(
                        "POST",
                        f"/api/magician/v2/hitl/{quote(request_id)}/respond",
                        body={"source": "user_request", "value": {"type": "text", "value": self.answer_text}},
                    )
                    self.answered.append({"id": request_id, "question": question, "answer": self.answer_text})
                except EvalFailure as error:
                    self.answered.append({"id": request_id, "question": question, "answer": f"respond failed: {error}"})


def cleanup_probe_task(client: Client, result: CaseResult, keep_artifacts: bool) -> None:
    task_id = result.artifacts.get("task_id")
    if keep_artifacts or not isinstance(task_id, str) or not task_id:
        return
    if result.artifacts.get("task_deleted_after_turn") is not None:
        # The case removed its own task mid-run (`delete_task_after_turn`);
        # there is nothing left to clean up.
        result.artifacts["task_deleted"] = True
        return
    try:
        delete_task(client, task_id)
        result.artifacts["task_deleted"] = True
    except RuntimeUnavailable:
        result.artifacts["task_deleted"] = False
    except EvalFailure as error:
        print(f"  cleanup: could not delete task {task_id}: {error}", file=sys.stderr)
        result.artifacts["task_deleted"] = False


def cleanup_probe_thread(client: Client, result: CaseResult, keep_artifacts: bool) -> None:
    thread_id = result.artifacts.get("thread_id")
    if keep_artifacts or not isinstance(thread_id, str) or not thread_id:
        return
    try:
        delete_thread(client, thread_id)
        result.artifacts["thread_deleted"] = True
    except RuntimeUnavailable:
        result.artifacts["thread_deleted"] = False
    except EvalFailure as error:
        print(f"  cleanup: could not delete thread {thread_id}: {error}", file=sys.stderr)
        result.artifacts["thread_deleted"] = False


def cleanup_probes(client: Client, result: CaseResult, keep_artifacts: bool) -> None:
    """Remove what the case left behind: the setup task or the task the mouth
    was asked to create, and the thread it was asked to create. Both deletes
    are lenient; a refused delete is printed and recorded, never a verdict."""
    cleanup_probe_task(client, result, keep_artifacts)
    cleanup_probe_thread(client, result, keep_artifacts)


def record_probe_left_behind(client: Client, result: CaseResult, effect: dict[str, Any], title: str) -> None:
    """After a turn failed, the effect check never runs, but the mouth may
    already have created the task or thread the case asked for. One lookup by
    title records its id so cleanup removes it; a lookup that fails leaves the
    probe behind, which is what `--keep-artifacts` would have done anyway."""
    try:
        if effect["kind"] == "task_exists" and not result.artifacts.get("task_id"):
            task = find_task_by_title(client, title)
            if task is not None:
                result.artifacts["task_id"] = task.get("id")
        elif effect["kind"] == "thread_exists" and not result.artifacts.get("thread_id"):
            thread = find_thread_by_title(client, title)
            if thread is not None:
                result.artifacts["thread_id"] = thread.get("id")
    except EvalFailure:
        return


def run_chat_case(
    client: Client,
    *,
    engine: str,
    baseline: bool,
    case: dict[str, Any],
    run: int,
    session_id: str,
    turn_timeout: float,
    keep_artifacts: bool,
) -> CaseResult:
    nonce = secrets.token_hex(4)
    values = {"nonce": nonce, "secret": f"kumquat-{secrets.token_hex(3)}", "task_id": "", "title": ""}
    result = CaseResult(
        lane="chat", engine=engine, case_id=case["id"], run=run, verdict="fail", reason="",
        effect_ok=None, proof_ok=None, effect_detail="", artifacts={"nonce": nonce},
    )
    effect = case["effect"]
    effect_turn = int(effect.get("turn", len(case["turns"])))
    # The title an existence effect looks for (`task_exists`, `thread_exists`),
    # rendered once so the effect check and the cleanup lookup agree.
    probe_title = render(str(effect.get("title") or ""), values)
    delete_after_turn = case.get("delete_task_after_turn")
    streamed = bool(case.get("stream"))
    withhold = case.get("turn_answer_must_not_contain")
    answers: list[str] = []
    proof_turns: list[bool] = []
    setup = case.get("setup")
    try:
        if setup is not None:
            values["title"] = render(str(setup.get("title", "HC-{nonce}")), values)
            values["task_id"] = create_probe_task(
                client, values["title"], render(str(setup.get("description", "")), values)
            )
            result.artifacts["task_id"] = values["task_id"]
        if isinstance(withhold, dict):
            # Rendered once here so the grader (live or regrade) checks the
            # same string the mouth was told to withhold.
            result.artifacts["withheld_value"] = render(str(withhold["value"]), values)
        for index, template in enumerate(case["turns"], start=1):
            prompt = render(template, values)
            chat_turn_id = f"hc-{engine}-{case['id']}-{nonce}-{index}"
            hitl_answer = render(str(case.get("hitl_answer") or "Yes, exactly as I said: {prompt}"), {**values, "prompt": prompt})
            turn_started_ms = int(time.time() * 1000)
            token_events = 0
            with HitlAnswerer(client, session_id, hitl_answer) as answerer:
                if streamed:
                    turn = stream_turn(client, session_id, prompt, chat_turn_id, turn_timeout)
                    if turn.errors:
                        result.artifacts.setdefault("stream_errors", []).extend(turn.errors)
                    if turn.failed():
                        raise EvalFailure(f"stream error: {turn.errors[0][:300]}")
                    answer, token_events, payload, latency = turn.answer, turn.token_events, turn.done, turn.latency_ms
                else:
                    payload, latency = client.json(
                        "POST",
                        f"/api/magician/v2/chat/sessions/{quote(session_id)}/messages",
                        body={"text": prompt, "chat_turn_id": chat_turn_id},
                        timeout=turn_timeout,
                    )
                    answer = message_text(payload.get("assistant_message") if isinstance(payload, dict) else None)
            if answerer.answered:
                result.artifacts.setdefault("hitl_answered", []).extend(answerer.answered)
            usage = (payload.get("usage") if isinstance(payload, dict) else None) or {}
            events = turn_events(client, session_id, chat_turn_id)
            harness_seen, tool_calls = harness_proof_from_events(events)
            for name in turn_tool_facts(client, chat_turn_id, turn_started_ms):
                if name not in tool_calls:
                    tool_calls.append(name)
            provider = usage.get("provider")
            record = TurnRecord(
                turn_index=index,
                chat_turn_id=chat_turn_id,
                prompt=prompt,
                answer=answer,
                provider=provider,
                model=usage.get("model"),
                cost_usd=float(usage.get("costUsd") or usage.get("cost_usd") or 0.0),
                input_tokens=int(usage.get("inputTokens") or usage.get("input_tokens") or 0),
                output_tokens=int(usage.get("outputTokens") or usage.get("output_tokens") or 0),
                latency_ms=latency,
                harness_event_seen=harness_seen,
                tool_calls=tool_calls,
                token_events=token_events,
            )
            result.turns.append(record)
            result.total_latency_ms += latency
            answers.append(answer)
            # Who answered: the response's usage row is written by the same
            # code path that chose the mouth, so it is the authoritative proof.
            # A streamed turn carries the same row on its `done` frame; when
            # that frame came back empty the turn's events still prove it.
            answered_by_harness = provider == "harness" or harness_seen
            proof_turns.append(answered_by_harness if not baseline else not answered_by_harness)
            if delete_after_turn == index and values["task_id"]:
                # The memory oracle: the task is gone before the next turn, so
                # a later answer can only come from what this turn brought back.
                delete_task_for_oracle(client, result, values["task_id"], index)
    except RuntimeUnavailable as error:
        result.verdict, result.reason = "inconclusive", str(error)
        cleanup_probes(client, result, keep_artifacts)
        return result
    except PreconditionFailure as error:
        # The effect was never measured (None, not False): the case did not
        # get far enough for the effect to mean anything.
        result.verdict, result.reason = "fail", f"precondition: {error}"
        cleanup_probes(client, result, keep_artifacts)
        return result
    except EvalFailure as error:
        result.verdict, result.reason = "fail", f"turn failed: {error}"
        result.effect_ok = False
        record_probe_left_behind(client, result, effect, probe_title)
        cleanup_probes(client, result, keep_artifacts)
        return result

    # Effect.
    title = probe_title
    try:
        if effect["kind"] == "answer_contains":
            needle = render(str(effect["value"]), values)
            answer = answers[effect_turn - 1] if 0 < effect_turn <= len(answers) else ""
            result.effect_ok = needle.lower() in answer.lower()
            result.effect_detail = f"looked for {needle!r} in turn {effect_turn}'s answer"
        elif effect["kind"] == "task_exists":
            task = wait_for_task(client, title, TASK_EFFECT_POLL_SECONDS)
            result.effect_ok = task is not None
            result.effect_detail = f"task titled {title!r} {'found' if task else 'not found'} in /v3/tasks"
            if task is not None:
                result.artifacts["task_id"] = task.get("id")
                result.artifacts["task_status"] = task.get("status")
        elif effect["kind"] == "thread_exists":
            thread = wait_for_thread(client, title, TASK_EFFECT_POLL_SECONDS)
            result.effect_ok = thread is not None
            result.effect_detail = f"thread titled {title!r} {'found' if thread else 'not found'} in /v2/ui-threads"
            if thread is not None:
                result.artifacts["thread_id"] = thread.get("id")
                result.artifacts["thread_name"] = thread.get("name")
        elif effect["kind"] == "task_field_contains":
            needle = render(str(effect["value"]), values)
            observed = wait_for_task_field(
                client, values["task_id"], str(effect["field"]), needle, TASK_EFFECT_POLL_SECONDS
            )
            result.effect_ok = needle.lower() in observed.lower()
            result.effect_detail = f"task {values['task_id']} {effect['field']}={observed!r}; wanted {needle!r}"
    except RuntimeUnavailable as error:
        result.verdict, result.reason = "inconclusive", str(error)
        cleanup_probes(client, result, keep_artifacts)
        return result
    cleanup_probes(client, result, keep_artifacts)
    result.proof_ok = all(proof_turns) if proof_turns else None
    grade_case_result(case, engine, baseline, result)
    return result


class ChatEngineSwitch:
    """Reads the live chat-engine selection and restores it on exit."""

    def __init__(self, client: Client):
        self.client = client
        self.saved_engine: str | None = None
        self.saved_model: str | None = None
        self.restore_error: str | None = None

    def save(self) -> dict[str, Any]:
        payload, _ = self.client.json("GET", "/api/magician/v2/plane/engines")
        self.saved_engine = str(payload.get("chat_current") or "magician")
        self.saved_model = str(payload.get("chat_model") or "default")
        return payload

    def select(self, engine: str) -> None:
        self.client.json(
            "PUT", "/api/magician/v2/plane/chat-engine", body={"harness_engine": engine}
        )

    def restore(self) -> None:
        if self.saved_engine is None:
            return
        try:
            self.client.json(
                "PUT",
                "/api/magician/v2/plane/chat-engine",
                body={"harness_engine": self.saved_engine, "harness_model": self.saved_model},
            )
        except EvalFailure as error:
            self.restore_error = str(error)


def select_cases(cases: list[dict[str, Any]], wanted_ids: list[str] | None) -> list[dict[str, Any]]:
    """The fixture cases `--case` keeps; an id the fixture lacks is an error,
    never an empty run that reads as green."""
    if not wanted_ids:
        return cases
    wanted = set(wanted_ids)
    missing = wanted - {c["id"] for c in cases}
    if missing:
        raise EvalFailure(f"unknown case ids: {sorted(missing)}")
    return [c for c in cases if c["id"] in wanted]


def roster_engines(roster_payload: dict[str, Any], requested: list[str] | None) -> tuple[dict[str, bool], list[str]]:
    """The engine roster `GET /plane/engines` reports (name -> installed) and
    the engines to run: `--engines` verbatim, else the `magician` baseline
    plus every installed roster engine."""
    roster = {
        str(e.get("name")): bool(e.get("installed"))
        for e in roster_payload.get("engines", [])
        if isinstance(e, dict) and e.get("name")
    }
    engines = list(requested) if requested else ["magician"] + [
        name for name in roster if name != "magician" and roster[name]
    ]
    unknown = [e for e in engines if e not in roster]
    if unknown:
        raise EvalFailure(f"engines not on the roster {sorted(roster)}: {unknown}")
    return roster, engines


def cli_unavailable_results(lane: str, engine: str, cases: list[dict[str, Any]], runs: int, note: str) -> list[CaseResult]:
    """One `cli_unavailable` result per case and run for an engine whose CLI
    probe failed, so the matrix keeps its shape."""
    return [
        CaseResult(
            lane=lane, engine=engine, case_id=case["id"], run=run,
            verdict="cli_unavailable", reason=note, effect_ok=None,
            proof_ok=None, effect_detail="",
        )
        for case in cases
        for run in range(1, runs + 1)
    ]


def run_chat_lane(args: argparse.Namespace) -> tuple[list[CaseResult], list[EngineSummary], dict[str, Any]]:
    client = Client(args.api_base_url, args.http_timeout_secs)
    cases = select_cases(load_cases(args.fixtures or FIXTURE_DIR / "chat_cases.json", "chat"), args.cases)

    switch = ChatEngineSwitch(client)
    roster, engines = roster_engines(switch.save(), args.engines)
    print(f"chat lane: engines={engines} cases={[c['id'] for c in cases]} runs={args.runs}")
    print(f"saved chat engine: {switch.saved_engine} (model {switch.saved_model})")

    results: list[CaseResult] = []
    summaries: list[EngineSummary] = []
    interrupted = False

    def on_sigint(signum: int, frame: Any) -> None:  # noqa: ARG001
        raise KeyboardInterrupt

    previous_handler = signal.signal(signal.SIGINT, on_sigint)
    try:
        for engine in engines:
            baseline = engine == "magician"
            probe_ok, probe_note = (True, "built-in") if baseline or args.skip_cli_probe else probe_cli(engine)
            print(f"[{engine}] cli probe: {probe_note}")
            counts = {v: 0 for v in VERDICTS}
            engine_latency = 0.0
            if not probe_ok:
                unavailable = cli_unavailable_results("chat", engine, cases, args.runs, probe_note)
                results.extend(unavailable)
                counts["cli_unavailable"] += len(unavailable)
                summaries.append(EngineSummary(engine, roster.get(engine, False), probe_note, counts, 0.0))
                continue
            try:
                switch.select(engine)
            except RuntimeUnavailable as error:
                print(f"[{engine}] runtime unavailable while switching: {error}")
                summaries.append(EngineSummary(engine, roster.get(engine, False), probe_note, counts, 0.0))
                break
            for run in range(1, args.runs + 1):
                # One fresh session per engine and run: continuity within a
                # multi-turn case is part of what is measured; continuity
                # across engines is not.
                try:
                    session_payload, _ = client.json(
                        "POST",
                        "/api/magician/v2/chat/new",
                        body={},
                        query={"ui_thread_id": f"hc-{engine}-{run}-{secrets.token_hex(3)}"},
                    )
                except RuntimeUnavailable as error:
                    print(f"[{engine}] runtime unavailable creating a session: {error}")
                    break
                session_id = str((session_payload.get("session") or {}).get("id") or "")
                if not session_id:
                    raise EvalFailure("POST /chat/new returned no session id")
                for case in cases:
                    result = run_chat_case(
                        client,
                        engine=engine,
                        baseline=baseline,
                        case=case,
                        run=run,
                        session_id=session_id,
                        turn_timeout=args.turn_timeout_secs,
                        keep_artifacts=args.keep_artifacts,
                    )
                    results.append(result)
                    counts[result.verdict] += 1
                    engine_latency += result.total_latency_ms
                    who = ",".join(sorted({t.provider or "?" for t in result.turns})) or "-"
                    frames = f" token_frames={sum(t.token_events for t in result.turns)}" if case.get("stream") else ""
                    print(
                        f"[{engine}] {case['id']}#{run}: {result.verdict} ({result.reason}); "
                        f"provider={who} latency={result.total_latency_ms/1000:.1f}s{frames}"
                    )
            summaries.append(EngineSummary(engine, roster.get(engine, False), probe_note, counts, engine_latency))
    except KeyboardInterrupt:
        interrupted = True
        print("interrupted; restoring the chat engine", file=sys.stderr)
    finally:
        signal.signal(signal.SIGINT, previous_handler)
        switch.restore()
        if switch.restore_error:
            print(
                f"RESTORE FAILED: chat engine may still be switched; set it back to "
                f"{switch.saved_engine!r} via PUT /plane/chat-engine ({switch.restore_error})",
                file=sys.stderr,
            )
        else:
            print(f"restored chat engine: {switch.saved_engine}")
    meta = {
        "saved_engine": switch.saved_engine,
        "saved_model": switch.saved_model,
        "restore_error": switch.restore_error,
        "interrupted": interrupted,
        "roster": roster,
    }
    return results, summaries, meta


# ---------------------------------------------------------------------------
# Run lane
# ---------------------------------------------------------------------------


def harness_provider_for(engine: str) -> str | None:
    """The provider a text-only operation rides when `engine` is the parent,
    or None for `magician`, which is never a parent."""
    family = ENGINE_PROVIDER_FAMILY.get(engine)
    return f"{HARNESS_PROVIDER_PREFIX}{family}" if family else None


def is_harness_provider(provider: Any) -> bool:
    return isinstance(provider, str) and provider.startswith(HARNESS_PROVIDER_PREFIX)


def is_tool_carrying(operation: Any) -> bool:
    return isinstance(operation, str) and operation.startswith(TOOL_CARRYING_OPERATION_PREFIX)


def execute_task(client: Client, task_id: str) -> str:
    """Start the task's run and return the execution id the route accepted.
    The body is optional; an empty one keeps the run on the task's own agent
    with no refinement and no client routing overrides, so the run engine
    under test is the only thing that differs between engines."""
    payload, _ = client.json(
        "POST", f"/api/magician/v3/tasks/{quote(task_id)}/execute", body={}, expected=(200, 201, 202)
    )
    execution = payload.get("execution") if isinstance(payload, dict) else None
    state = execution.get("state") if isinstance(execution, dict) else None
    execution_id = state.get("execution_id") if isinstance(state, dict) else None
    if not isinstance(execution_id, str) or not execution_id.strip():
        raise EvalFailure(f"execute response has no execution id: {json.dumps(payload)[:300]}")
    return execution_id.strip()


def read_execution_status(client: Client, task_id: str, execution_id: str) -> str:
    payload, _ = client.json(
        "GET", f"/api/magician/v3/tasks/{quote(task_id)}/executions/{quote(execution_id)}"
    )
    state = payload.get("state") if isinstance(payload, dict) else None
    return str(state.get("status") or "") if isinstance(state, dict) else ""


def wait_for_run(
    client: Client, task_id: str, execution_id: str, deadline_seconds: float, poll_seconds: float = RUN_POLL_SECONDS
) -> tuple[str, str]:
    """Poll the run to rest. Returns the outcome and the last status read:
    `terminal`; `waiting` when two consecutive reads found the run in a state
    that needs a person (one read can catch a pause the driver is already
    resuming, and a false stop would throw away a multi-minute run); or
    `timeout` when the deadline passed first."""
    deadline = time.monotonic() + deadline_seconds
    waiting_streak = 0
    while True:
        status = read_execution_status(client, task_id, execution_id)
        if status in RUN_TERMINAL_STATUSES:
            return "terminal", status
        waiting_streak = waiting_streak + 1 if status in RUN_WAITING_STATUSES else 0
        if waiting_streak >= 2:
            return "waiting", status
        if time.monotonic() >= deadline:
            return "timeout", status
        time.sleep(poll_seconds)


def cancel_run(client: Client, result: CaseResult, execution_id: str) -> None:
    """Stop a run the case is done with. Lenient: a refused cancel is printed
    and recorded, since the state that led here already decided the verdict."""
    try:
        client.json(
            "POST",
            f"/api/magician/v3/executions/{quote(execution_id)}/cancel",
            body={},
            expected=(200, 202, 204, 404, 409),
        )
        result.artifacts["run_cancelled"] = True
    except RuntimeUnavailable:
        result.artifacts["run_cancelled"] = False
    except EvalFailure as error:
        print(f"  cleanup: could not cancel execution {execution_id}: {error}", file=sys.stderr)
        result.artifacts["run_cancelled"] = False


def read_events(client: Client, query: dict[str, Any]) -> list[dict[str, Any]]:
    """One backfill read of the event journal, NDJSON in, rows out."""
    status, raw, _ = client.request("GET", "/api/magician/v3/events", query=query)
    if status != 200:
        raise EvalFailure(f"GET /api/magician/v3/events -> HTTP {status}")
    events: list[dict[str, Any]] = []
    for line in raw.decode("utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as error:
            raise EvalFailure("invalid JSON line in the run's event journal") from error
        if isinstance(row, dict):
            events.append(row)
    return events


def run_events(client: Client, task_id: str, execution_id: str, started_ms: int) -> list[dict[str, Any]]:
    """The run's canonical journal, one event per NDJSON line. Backfill only,
    so the response ends when the journal is drained instead of tailing.

    Naming both the task and the execution asks the route for the single
    on-disk journal; naming only the execution makes it walk the scope. The
    first is cheaper and exact, but it answers empty on a store where the
    journal is not where that path expects it, and an unread journal is
    indistinguishable from a run that emitted nothing. So an empty first read
    is retried across the scope before the proof is called missing."""
    base = {"since": started_ms, "limit": RUN_EVENTS_LIMIT, "backfill_only": "true"}
    events = read_events(client, {"task_id": task_id, "execution_id": execution_id, **base})
    if events:
        return events
    return read_events(client, {"execution_id": execution_id, **base})


@dataclass
class RunProof:
    """What the run's journal says about who decided its turns."""

    # Every event read; 0 means the journal itself could not be read.
    events: int
    # The engine each `harness_turn_settled` fact names, in journal order.
    harness_engines: list[str]
    # `llm.succeeded` facts on a tool-carrying operation: the native loop
    # deciding a turn itself.
    native_decisions: int
    # Distinct tool targets that succeeded, for the report.
    hands: list[str]


def read_run_proof(events: list[dict[str, Any]]) -> RunProof:
    harness_engines: list[str] = []
    hands: list[str] = []
    native_decisions = 0
    for event in events:
        kind, payload = inner_event(event)
        if kind == "execution.progress" and payload.get("kind") == "harness_turn_settled":
            harness_engines.append(str(payload.get("engine") or ""))
        elif kind == "llm.succeeded" and is_tool_carrying(payload.get("operation")):
            native_decisions += 1
        elif kind == "tool.succeeded":
            target = str(payload.get("target") or payload.get("tool_name") or "")
            name = target.split("(", 1)[0].strip()
            if name and name not in hands:
                hands.append(name)
    return RunProof(len(events), harness_engines, native_decisions, hands)


def run_engine_proof(proof: RunProof, engine: str) -> tuple[bool | None, str]:
    """Whether the engine under test drove the run's decide turns. An
    external engine leaves one `harness_turn_settled` fact per turn naming
    itself; `magician` leaves none, and its native loop's decisions show as
    `llm.succeeded` on the tool-carrying operation. None when the journal
    holds neither kind of fact, so the verdict says the proof is missing
    rather than wrong."""
    named = sorted(set(proof.harness_engines))
    if proof.events == 0:
        return None, "run journal empty or unread"
    if engine == "magician":
        if proof.harness_engines:
            return False, f"harness_turn_settled named {named}"
        if proof.native_decisions:
            return True, f"{proof.native_decisions} native decision call(s), no harness turn"
        return None, "no decision fact in the run journal"
    if not proof.harness_engines:
        return False, f"no harness_turn_settled fact; {proof.native_decisions} native decision call(s)"
    if named != [engine]:
        return False, f"harness_turn_settled named {named}, wanted {engine!r}"
    return True, f"{len(proof.harness_engines)} harness turn(s) settled on {engine}"


def llm_fact_rows(client: Client, sql: str, started_ms: int) -> list[dict[str, Any]]:
    """Rows from the parser-validated fact query, bounded to the window the
    run could have written in (a little before it started, generously after).
    Raises on a refused or malformed query; the caller records that apart
    from a query that simply found nothing yet."""
    payload, _ = client.json(
        "POST",
        "/api/magician/v2/analytics/llm/facts/query",
        body={"sql": sql, "from_ms": started_ms - 5000, "to_ms": int(time.time() * 1000) + 60000, "limit": 500},
        expected=(200,),
        timeout=90.0,
    )
    rows = (payload.get("data") or {}).get("rows") if isinstance(payload, dict) else None
    return [r for r in rows or [] if isinstance(r, dict)]


def task_llm_facts_sql(task_id: str) -> str:
    safe = "".join(ch for ch in task_id if ch.isalnum() or ch in "_-.:")
    return (
        "SELECT operation, profile, provider, model, success, execution_id, timestamp_ms "
        f"FROM llm_calls WHERE task_id = '{safe}'"
    )


WINDOW_LLM_FACTS_SQL = (
    "SELECT task_id, execution_id, operation, profile, provider, model, success, timestamp_ms FROM llm_calls"
)


def operations_seen(rows: list[dict[str, Any]]) -> dict[str, list[str]]:
    """Operation -> the providers it ran on, both sorted, for the report and
    the reasons."""
    seen: dict[str, set[str]] = {}
    for row in rows:
        seen.setdefault(str(row.get("operation") or "?"), set()).add(str(row.get("provider") or "?"))
    return {operation: sorted(providers) for operation, providers in sorted(seen.items())}


def format_operations(seen: dict[str, list[str]]) -> str:
    return ", ".join(f"{operation}={'/'.join(providers)}" for operation, providers in seen.items()) or "none"


def task_llm_facts(
    client: Client, result: CaseResult, task_id: str, started_ms: int, deadline_seconds: float
) -> list[dict[str, Any]]:
    """The `llm_calls` facts attributed to the task, polled until at least one
    appears or the deadline passes (facts materialise behind the run), plus
    every fact in the same window with no task filter, so an operation that
    ran outside the task's trace context is still visible in the report. The
    rows, the SQL and any query error land on the result's artifacts."""
    task_sql = task_llm_facts_sql(task_id)
    deadline = time.monotonic() + deadline_seconds
    rows: list[dict[str, Any]] = []
    while True:
        try:
            rows = llm_fact_rows(client, task_sql, started_ms)
        except RuntimeUnavailable:
            raise
        except EvalFailure as error:
            result.artifacts["analytics_error"] = str(error)
            rows = []
            break
        if rows or time.monotonic() >= deadline:
            break
        time.sleep(ANALYTICS_POLL_INTERVAL_SECONDS)
    try:
        window_rows = llm_fact_rows(client, WINDOW_LLM_FACTS_SQL, started_ms)
    except RuntimeUnavailable:
        raise
    except EvalFailure as error:
        result.artifacts.setdefault("analytics_error", str(error))
        window_rows = []
    rows.sort(key=lambda r: (r.get("timestamp_ms") or 0))
    window_rows.sort(key=lambda r: (r.get("timestamp_ms") or 0))
    seen = operations_seen(rows)
    result.artifacts["analytics_sql"] = task_sql
    result.artifacts["analytics_rows"] = rows
    result.artifacts["window_rows"] = window_rows
    result.artifacts["operations_seen"] = seen
    result.artifacts["providers_seen"] = sorted({p for providers in seen.values() for p in providers})
    task_providers = set(result.artifacts["providers_seen"])
    result.artifacts["window_harness_providers"] = sorted(
        {str(r.get("provider")) for r in window_rows if is_harness_provider(r.get("provider"))} - task_providers
    )
    return rows


@dataclass
class TraceCheck:
    """The parent-engine rule read off the task's `llm_calls` facts."""

    # The floor held: no tool-carrying operation rode a harness provider (and
    # on the baseline, no operation did). None when no row was read.
    ok: bool | None
    # The rule held: a text-only operation rode the parent's provider. None
    # on the baseline (nothing to follow) or when no row was read.
    followed: bool | None
    text_ops: int
    seen: dict[str, list[str]]
    detail: str


def trace_check(rows: list[dict[str, Any]], engine: str) -> TraceCheck:
    seen = operations_seen(rows)
    summary = format_operations(seen)
    if not rows:
        return TraceCheck(None, None, 0, seen, "no llm_calls row attributed to the task")
    if engine == "magician":
        strays = sorted({str(r.get("operation")) for r in rows if is_harness_provider(r.get("provider"))})
        if strays:
            return TraceCheck(False, None, 0, seen, f"{strays} rode a harness provider; seen {summary}")
        return TraceCheck(True, None, 0, seen, f"no harness provider in the trace; seen {summary}")
    parent = harness_provider_for(engine)
    rode = sorted(
        {str(r.get("operation")) for r in rows if is_tool_carrying(r.get("operation")) and is_harness_provider(r.get("provider"))}
    )
    text_rows = [r for r in rows if not is_tool_carrying(r.get("operation"))]
    followed = any(r.get("provider") == parent for r in text_rows)
    if rode:
        return TraceCheck(False, followed, len(text_rows), seen, f"{rode} rode a harness provider; seen {summary}")
    if not text_rows:
        return TraceCheck(True, False, 0, seen, f"seen {summary}")
    if not followed:
        return TraceCheck(True, False, len(text_rows), seen, f"none rode {parent}; seen {summary}")
    return TraceCheck(True, True, len(text_rows), seen, f"text-only operations rode {parent}; seen {summary}")


def grade_run_result(case: dict[str, Any], engine: str, baseline: bool, result: CaseResult) -> None:
    """The run lane's one grader, live and regrade alike: from a result whose
    run outcome, effect, proof and analytics rows are on it, derive the trace
    checks, the verdict and its reason. A run that stopped for a person or
    ran out the deadline is decided here too, so a regrade cannot promote it
    on an effect the stopped run happened to leave."""
    outcome = result.artifacts.get("run_outcome")
    status = result.artifacts.get("run_status")
    if outcome == "waiting":
        result.verdict = "fail"
        result.reason = f"run_waited_for_input: the run stopped in {status!r} and the lane has no one to answer it"
        return
    if outcome == "timeout":
        result.verdict = "inconclusive"
        result.reason = (
            f"run_timeout: the run was still {status!r} when the "
            f"{result.artifacts.get('run_deadline_secs')}s deadline passed"
        )
        return
    rows = result.artifacts.get("analytics_rows") or []
    trace = trace_check(rows, engine) if case.get("trace") else None
    result.trace_ok = trace.ok if trace else None
    result.trace_followed = trace.followed if trace else None
    result.verdict, result.reason = decide_verdict(
        effect_ok=result.effect_ok,
        proof_ok=result.proof_ok,
        baseline=baseline,
        partial_on_effect_miss=False,
        cli_available=True,
        inconclusive=False,
        trace_rows=len(rows) if trace else None,
        trace_ok=result.trace_ok,
        trace_followed=result.trace_followed,
    )
    if trace is None:
        return
    if result.reason == TRACE_NO_TEXT_OP_ON_PARENT and trace.text_ops == 0:
        result.reason = TRACE_NO_TEXT_OP_OBSERVED
    if result.reason in (
        TRACE_TOOL_CALL_RODE_HARNESS, TRACE_BASELINE_RODE_HARNESS, TRACE_NO_TEXT_OP_ON_PARENT, TRACE_NO_TEXT_OP_OBSERVED,
    ):
        result.reason = f"{result.reason}: {trace.detail}"
    elif result.reason == TRACE_NO_ANALYTICS_ROWS and result.artifacts.get("analytics_error"):
        result.reason = f"{result.reason}: {result.artifacts['analytics_error']}"
    strays = result.artifacts.get("window_harness_providers") or []
    if strays and result.reason.startswith((TRACE_NO_TEXT_OP_ON_PARENT, TRACE_NO_TEXT_OP_OBSERVED)):
        # A harness provider that ran in the window but outside the task's
        # trace context is the likeliest explanation for a rule that reads as
        # unfollowed; naming it here saves a trip to the window rows.
        result.reason = f"{result.reason}; the window holds {strays} outside the task's trace context"


def run_run_case(
    client: Client,
    *,
    engine: str,
    baseline: bool,
    case: dict[str, Any],
    run: int,
    run_timeout: float,
    keep_artifacts: bool,
    poll_seconds: float = RUN_POLL_SECONDS,
    analytics_deadline: float = ANALYTICS_POLL_SECONDS,
) -> CaseResult:
    """One task, one run, under the engine already selected: create the probe
    task with the case's description, execute it, wait for it to rest, then
    read the effect off the task, the proof off the run's journal and the
    trace off the LLM facts. A run that did not reach a terminal status is
    cancelled before its task is deleted, and its effect and facts are read
    once without waiting, for the record."""
    nonce = secrets.token_hex(4)
    values = {"nonce": nonce, "title": f"HC-{nonce}", "task_id": ""}
    result = CaseResult(
        lane="run", engine=engine, case_id=case["id"], run=run, verdict="fail", reason="",
        effect_ok=None, proof_ok=None, effect_detail="",
        artifacts={"nonce": nonce, "run_deadline_secs": run_timeout},
    )
    effect = case["effect"]
    started_ms = int(time.time() * 1000)
    started = time.perf_counter()
    execution_id = ""
    try:
        values["task_id"] = create_probe_task(client, values["title"], render(str(case["description"]), values))
        result.artifacts["task_id"] = values["task_id"]
        execution_id = execute_task(client, values["task_id"])
        result.artifacts["execution_id"] = execution_id
        try:
            outcome, status = wait_for_run(client, values["task_id"], execution_id, run_timeout, poll_seconds)
        except KeyboardInterrupt:
            # An interrupted lane must not leave a probe running on the
            # switched engine after the engine is restored.
            cancel_run(client, result, execution_id)
            cleanup_probe_task(client, result, keep_artifacts)
            raise
        result.total_latency_ms = (time.perf_counter() - started) * 1000
        result.artifacts["run_outcome"], result.artifacts["run_status"] = outcome, status
        if outcome != "terminal":
            cancel_run(client, result, execution_id)
    except RuntimeUnavailable as error:
        result.verdict, result.reason = "inconclusive", str(error)
        cleanup_probe_task(client, result, keep_artifacts)
        return result
    except EvalFailure as error:
        result.verdict, result.reason = "fail", f"run failed to start: {error}"
        result.effect_ok = False
        cleanup_probe_task(client, result, keep_artifacts)
        return result

    settled = outcome == "terminal"
    try:
        # Effect: the run's own task, read after the run rested. A run that
        # never settled is read once; its verdict is already decided.
        needle = render(str(effect["value"]), values)
        observed = wait_for_task_field(
            client, values["task_id"], str(effect["field"]), needle,
            TASK_EFFECT_POLL_SECONDS if settled else 0.0,
        )
        result.effect_ok = needle.lower() in observed.lower()
        result.effect_detail = f"task {values['task_id']} {effect['field']}={observed!r}; wanted {needle!r}; run {status}"
        # Proof: who decided the run's turns, from its journal.
        try:
            events = run_events(client, values["task_id"], execution_id, started_ms)
        except RuntimeUnavailable:
            raise
        except EvalFailure as error:
            events = []
            result.artifacts["events_error"] = str(error)
        proof = read_run_proof(events)
        result.artifacts["events_read"] = proof.events
        result.artifacts["harness_engines_seen"] = sorted(set(proof.harness_engines))
        result.artifacts["native_decisions"] = proof.native_decisions
        result.artifacts["hands"] = proof.hands
        result.proof_ok, proof_detail = run_engine_proof(proof, engine)
        result.effect_detail = f"{result.effect_detail}; proof: {proof_detail}"
        # Trace: the operations the run put through the router, and where.
        task_llm_facts(client, result, values["task_id"], started_ms, analytics_deadline if settled else 0.0)
        result.effect_detail = f"{result.effect_detail}; trace: {format_operations(result.artifacts['operations_seen'])}"
    except RuntimeUnavailable as error:
        result.verdict, result.reason = "inconclusive", str(error)
        cleanup_probe_task(client, result, keep_artifacts)
        return result
    cleanup_probe_task(client, result, keep_artifacts)
    grade_run_result(case, engine, baseline, result)
    return result


class RunEngineSwitch:
    """Reads the live run-engine selection (`execution.harness_engine` and its
    model) and restores both on exit. The PUT treats an absent `harness_model`
    as `default`, so the restore sends the saved model too, or a pinned model
    would be lost with the switch."""

    def __init__(self, client: Client):
        self.client = client
        self.saved_engine: str | None = None
        self.saved_model: str | None = None
        self.restore_error: str | None = None

    def save(self) -> dict[str, Any]:
        payload, _ = self.client.json("GET", "/api/magician/v2/plane/engines")
        self.saved_engine = str(payload.get("current") or "magician")
        self.saved_model = str(payload.get("run_model") or "default")
        return payload

    def select(self, engine: str) -> None:
        self.client.json("PUT", "/api/magician/v2/plane/engine", body={"harness_engine": engine})

    def restore(self) -> None:
        if self.saved_engine is None:
            return
        try:
            self.client.json(
                "PUT",
                "/api/magician/v2/plane/engine",
                body={"harness_engine": self.saved_engine, "harness_model": self.saved_model},
            )
        except EvalFailure as error:
            self.restore_error = str(error)


def run_run_lane(args: argparse.Namespace) -> tuple[list[CaseResult], list[EngineSummary], dict[str, Any]]:
    client = Client(args.api_base_url, args.http_timeout_secs)
    cases = select_cases(load_cases(args.fixtures or FIXTURE_DIR / "run_cases.json", "run"), args.cases)

    switch = RunEngineSwitch(client)
    roster, engines = roster_engines(switch.save(), args.engines)
    print(f"run lane: engines={engines} cases={[c['id'] for c in cases]} runs={args.runs}")
    print(f"saved run engine: {switch.saved_engine} (model {switch.saved_model})")

    results: list[CaseResult] = []
    summaries: list[EngineSummary] = []
    interrupted = False

    def on_sigint(signum: int, frame: Any) -> None:  # noqa: ARG001
        raise KeyboardInterrupt

    previous_handler = signal.signal(signal.SIGINT, on_sigint)
    try:
        for engine in engines:
            baseline = engine == "magician"
            probe_ok, probe_note = (True, "built-in") if baseline or args.skip_cli_probe else probe_cli(engine)
            print(f"[{engine}] cli probe: {probe_note}")
            counts = {v: 0 for v in VERDICTS}
            engine_latency = 0.0
            if not probe_ok:
                unavailable = cli_unavailable_results("run", engine, cases, args.runs, probe_note)
                results.extend(unavailable)
                counts["cli_unavailable"] += len(unavailable)
                summaries.append(EngineSummary(engine, roster.get(engine, False), probe_note, counts, 0.0))
                continue
            try:
                switch.select(engine)
            except RuntimeUnavailable as error:
                print(f"[{engine}] runtime unavailable while switching: {error}")
                summaries.append(EngineSummary(engine, roster.get(engine, False), probe_note, counts, 0.0))
                break
            for run in range(1, args.runs + 1):
                for case in cases:
                    result = run_run_case(
                        client,
                        engine=engine,
                        baseline=baseline,
                        case=case,
                        run=run,
                        run_timeout=args.turn_timeout_secs,
                        keep_artifacts=args.keep_artifacts,
                    )
                    results.append(result)
                    counts[result.verdict] += 1
                    engine_latency += result.total_latency_ms
                    providers = ",".join(result.artifacts.get("providers_seen") or []) or "-"
                    print(
                        f"[{engine}] {case['id']}#{run}: {result.verdict} ({result.reason}); "
                        f"providers={providers} run={result.artifacts.get('run_status') or '-'} "
                        f"latency={result.total_latency_ms/1000:.1f}s"
                    )
            summaries.append(EngineSummary(engine, roster.get(engine, False), probe_note, counts, engine_latency))
    except KeyboardInterrupt:
        interrupted = True
        print("interrupted; restoring the run engine", file=sys.stderr)
    finally:
        signal.signal(signal.SIGINT, previous_handler)
        switch.restore()
        if switch.restore_error:
            print(
                f"RESTORE FAILED: run engine may still be switched; set it back to "
                f"{switch.saved_engine!r} (model {switch.saved_model!r}) via PUT /plane/engine ({switch.restore_error})",
                file=sys.stderr,
            )
        else:
            print(f"restored run engine: {switch.saved_engine} (model {switch.saved_model})")
    meta = {
        "saved_engine": switch.saved_engine,
        "saved_model": switch.saved_model,
        "restore_error": switch.restore_error,
        "interrupted": interrupted,
        "roster": roster,
    }
    return results, summaries, meta


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------


def report_payload(
    lane: str, mode: str, results: list[CaseResult], summaries: list[EngineSummary], meta: dict[str, Any]
) -> dict[str, Any]:
    overall = {v: 0 for v in VERDICTS}
    for result in results:
        overall[result.verdict] += 1
    scored = [s for s in summaries if s.cli_probe not in ("",)]
    lane_ok = overall["fail"] == 0 and overall["inconclusive"] == 0 and any(
        s.verdict_counts.get("pass", 0) > 0 for s in scored
    )
    return {
        "lane": lane,
        "mode": mode,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "summary": {
            "verdicts": overall,
            "ok": lane_ok,
            "engines": [
                {
                    "engine": s.engine,
                    "installed": s.installed,
                    "cli_probe": s.cli_probe,
                    "verdicts": s.verdict_counts,
                    "total_latency_ms": round(s.total_latency_ms, 1),
                    "line": f"{s.engine}: {s.verdict_counts['pass']} pass / {s.verdict_counts['partial']} partial / "
                    f"{s.verdict_counts['fail']} fail"
                    + (f" / {s.verdict_counts['cli_unavailable']} cli_unavailable" if s.verdict_counts["cli_unavailable"] else "")
                    + (f" / {s.verdict_counts['inconclusive']} inconclusive" if s.verdict_counts["inconclusive"] else ""),
                }
                for s in summaries
            ],
        },
        "meta": meta,
        "results": [asdict(r) for r in results],
    }


def write_report(output_dir: Path, payload: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    engine_rows = "\n".join(
        f"<tr><td>{escape(e['engine'])}</td><td>{escape(e['cli_probe'])}</td>"
        f"<td>{e['verdicts']['pass']}</td><td>{e['verdicts']['partial']}</td><td>{e['verdicts']['fail']}</td>"
        f"<td>{e['verdicts']['cli_unavailable']}</td><td>{e['verdicts']['inconclusive']}</td>"
        f"<td>{e['total_latency_ms']/1000:.1f}s</td></tr>"
        for e in payload["summary"]["engines"]
    )
    def case_row(r: dict[str, Any]) -> str:
        # A chat result carries its turns; a run result carries its
        # artifacts (the run's status, the proof read off its journal and the
        # trace rows), so the same columns read from whichever is there. The
        # unfiltered window rows stay in the JSON only: they can be hundreds.
        artifacts = r.get("artifacts") or {}
        providers = ", ".join(sorted({t["provider"] or "?" for t in r["turns"]})) or ", ".join(
            artifacts.get("providers_seen") or []
        ) or "-"
        tools = ", ".join(sorted({n for t in r["turns"] for n in t["tool_calls"]})) or ", ".join(
            artifacts.get("hands") or []
        ) or "-"
        if r["turns"]:
            label = "turns"
            evidence = json.dumps(
                [{"prompt": t["prompt"], "answer": t["answer"][:600], "token_events": t.get("token_events", 0)} for t in r["turns"]],
                indent=1,
            )
        else:
            label = "artifacts"
            shown = {k: v for k, v in artifacts.items() if k != "window_rows"}
            if "window_rows" in artifacts:
                shown["window_rows"] = f"{len(artifacts['window_rows'])} row(s) in report.json"
            evidence = json.dumps(shown, indent=1, sort_keys=True)
        latency = f"{r['total_latency_ms'] / 1000:.1f}s"
        return (
            "<tr>"
            f"<td>{escape(r['engine'])}</td><td>{escape(r['case_id'])}#{r['run']}</td>"
            f"<td class='v-{escape(r['verdict'])}'>{escape(r['verdict'])}</td>"
            f"<td>{escape(r['reason'])}</td>"
            f"<td>{escape(providers)}</td>"
            f"<td>{escape(tools)}</td>"
            f"<td>{latency}</td>"
            f"<td>{escape(r['effect_detail'])}</td>"
            f"<td><details><summary>{label}</summary><pre>{escape(evidence)}</pre></details></td>"
            "</tr>"
        )

    case_rows = "\n".join(case_row(r) for r in payload["results"])
    html = f"""<!doctype html><html><head><meta charset="utf-8">
<title>Harness conformance — {escape(payload['lane'])} lane</title>
<style>body{{font:14px/1.4 -apple-system,Helvetica,Arial,sans-serif;margin:2rem;color:#222}}
table{{border-collapse:collapse;margin:1rem 0}} td,th{{border:1px solid #ccc;padding:.3rem .5rem;text-align:left;vertical-align:top}}
.v-pass{{color:#0a7}} .v-partial{{color:#b80}} .v-fail{{color:#c22}} .v-cli_unavailable,.v-inconclusive{{color:#888}}
pre{{white-space:pre-wrap;max-width:60ch}}</style></head><body>
<h1>Harness conformance — {escape(payload['lane'])} lane ({escape(payload['mode'])})</h1>
<p>Generated {escape(payload['generated_at'])}. Overall: {escape(json.dumps(payload['summary']['verdicts']))}; lane ok: {payload['summary']['ok']}.</p>
<p>Saved engine before the run: {escape(str(payload['meta'].get('saved_engine')))}; restore error: {escape(str(payload['meta'].get('restore_error')))}.</p>
<h2>Per engine</h2>
<table><tr><th>engine</th><th>cli probe</th><th>pass</th><th>partial</th><th>fail</th><th>cli unavailable</th><th>inconclusive</th><th>latency</th></tr>
{engine_rows}</table>
<h2>Per case</h2>
<table><tr><th>engine</th><th>case</th><th>verdict</th><th>reason</th><th>provider</th><th>tool calls seen</th><th>latency</th><th>effect</th><th>evidence</th></tr>
{case_rows}</table>
<p><a href="report.json">Raw JSON evidence</a></p></body></html>"""
    (output_dir / "report.html").write_text(html, encoding="utf-8")


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------


def self_test_chat(args: argparse.Namespace) -> int:
    cases = load_cases(FIXTURE_DIR / "chat_cases.json", "chat")
    assert [c["id"] for c in cases] == [
        "pong", "task_lookup", "task_update", "agent_roster", "remember_recall", "tool_result_recall", "streaming",
        "thread_create", "create_task",
    ], cases
    by_id = {c["id"]: c for c in cases}
    recall = by_id["tool_result_recall"]
    assert recall["delete_task_after_turn"] == 1 and recall["no_tools_turn"] == 2 and recall["effect"]["turn"] == 2, recall
    assert recall["turn_answer_must_not_contain"] == {"turn": 1, "value": "{secret}"}, recall
    assert "{secret}" not in recall["turns"][0] and "DONE" in recall["turns"][0], recall["turns"]
    streaming = by_id["streaming"]
    assert streaming["stream"] is True and streaming["min_token_events"] == 2 and streaming["engines_exempt"] == ["codex"], streaming
    # The bridged-tool cases: no setup, the mouth creates the probe itself,
    # and the existence effect names the title the rig then looks for.
    thread_create, create_task = by_id["thread_create"], by_id["create_task"]
    assert thread_create["effect"] == {"kind": "thread_exists", "title": "HC-{nonce}"} and "setup" not in thread_create, thread_create
    assert create_task["effect"] == {"kind": "task_exists", "title": "HC-{nonce}"} and "setup" not in create_task, create_task
    assert thread_create["tools_expected"] is True and create_task["tools_expected"] is True
    assert "hc-{nonce}" in thread_create["turns"][0] and "HC-{nonce}" in create_task["turns"][0]
    # The turn options are validated against the case's own turn count.
    bad_fixture = args.output_dir / "self-test-bad-cases.json"
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for bad in (
        {"no_tools_turn": 3},
        {"no_tools_turn": "2"},
        {"delete_task_after_turn": 0},
        {"turn_answer_must_not_contain": {"turn": 3, "value": "x"}},
        {"turn_answer_must_not_contain": {"turn": "1", "value": "x"}},
        {"turn_answer_must_not_contain": {"turn": 1}},
        {"turn_answer_must_not_contain": {"turn": 1, "value": ""}},
        {"turn_answer_must_not_contain": "x"},
    ):
        bad_fixture.write_text(json.dumps({"lane": "chat", "cases": [{
            "id": "x", "turns": ["a", "b"], "effect": {"kind": "answer_contains", "value": "a"},
            "setup": {"kind": "create_task"}, **bad,
        }]}), encoding="utf-8")
        try:
            load_cases(bad_fixture, "chat")
        except EvalFailure:
            pass
        else:
            raise AssertionError(f"load_cases accepted {bad}")
    bad_fixture.write_text(json.dumps({"lane": "chat", "cases": [{
        "id": "x", "turns": ["a"], "effect": {"kind": "answer_contains", "value": "a"}, "delete_task_after_turn": 1,
    }]}), encoding="utf-8")
    try:
        load_cases(bad_fixture, "chat")
    except EvalFailure:
        pass
    else:
        raise AssertionError("load_cases accepted delete_task_after_turn without a setup")
    # The streaming options: a boolean, a positive integer that needs the
    # stream, and a list of roster engines that needs the bound.
    for bad in (
        {"stream": "yes"},
        {"stream": True, "min_token_events": 0},
        {"stream": True, "min_token_events": True},
        {"stream": True, "min_token_events": "2"},
        {"min_token_events": 2},
        {"stream": True, "min_token_events": 2, "engines_exempt": "codex"},
        {"stream": True, "min_token_events": 2, "engines_exempt": ["not-an-engine"]},
        {"stream": True, "engines_exempt": ["codex"]},
    ):
        bad_fixture.write_text(json.dumps({"lane": "chat", "cases": [{
            "id": "x", "turns": ["a"], "effect": {"kind": "answer_contains", "value": "a"}, **bad,
        }]}), encoding="utf-8")
        try:
            load_cases(bad_fixture, "chat")
        except EvalFailure:
            pass
        else:
            raise AssertionError(f"load_cases accepted {bad}")
    bad_fixture.write_text(json.dumps({"lane": "chat", "cases": [{
        "id": "x", "turns": ["a"], "effect": {"kind": "answer_contains", "value": "a"},
        "stream": True, "min_token_events": 2, "engines_exempt": ["codex", "magician"],
    }]}), encoding="utf-8")
    assert load_cases(bad_fixture, "chat")[0]["engines_exempt"] == ["codex", "magician"]
    # The existence effects: the thread kind is accepted, and both kinds need
    # the title they look for.
    for bad in (
        {"kind": "thread_exists"},
        {"kind": "thread_exists", "title": ""},
        {"kind": "thread_exists", "title": 3},
        {"kind": "task_exists"},
        {"kind": "channel_exists", "title": "x"},
    ):
        bad_fixture.write_text(json.dumps({"lane": "chat", "cases": [{"id": "x", "turns": ["a"], "effect": bad}]}), encoding="utf-8")
        try:
            load_cases(bad_fixture, "chat")
        except EvalFailure:
            pass
        else:
            raise AssertionError(f"load_cases accepted the effect {bad}")
    for kind in ("thread_exists", "task_exists"):
        bad_fixture.write_text(json.dumps({"lane": "chat", "cases": [{
            "id": "x", "turns": ["a"], "effect": {"kind": kind, "title": "HC-{nonce}"}, "tools_expected": True,
        }]}), encoding="utf-8")
        assert load_cases(bad_fixture, "chat")[0]["effect"]["kind"] == kind
    bad_fixture.unlink()
    # The thread match: the display name case-insensitively, or the id the
    # title slugs to, so a mouth that set only the id still counts.
    assert thread_slug("HC-Ab12cd34") == "hc-ab12cd34" and thread_slug("  Plans / Q3  ") == "plans-q3"
    assert thread_matches_title({"id": "hc-ab12cd34", "name": "HC-Ab12cd34"}, "HC-Ab12cd34")
    assert thread_matches_title({"id": "hc-ab12cd34", "name": "hc-ab12cd34"}, "HC-Ab12cd34")
    assert thread_matches_title({"id": "probe", "name": " hc-ab12cd34 "}, "HC-Ab12cd34")
    assert not thread_matches_title({"id": "hc-ab12cd34-2", "name": "HC-Ab12cd34 (2)"}, "HC-Ab12cd34")
    assert not thread_matches_title({"id": "general", "name": "General"}, "HC-Ab12cd34")
    assert not thread_matches_title({}, "HC-Ab12cd34")
    # The thread lookup on a canned list response: the UI-thread route, its
    # `q` filter, and the row the match returns.
    class StubThreads:
        def __init__(self, threads: list[dict[str, Any]]):
            self.threads = threads
            self.calls: list[tuple[str, str, dict[str, Any] | None]] = []

        def json(self, method: str, path: str, body: Any = None, query: dict[str, Any] | None = None, **_: Any) -> tuple[Any, float]:
            self.calls.append((method, path, query))
            return {"threads": self.threads, "total": len(self.threads), "limit": THREAD_LIST_PAGE_LIMIT, "offset": 0}, 1.0

    stub = StubThreads([{"id": "general", "name": "General", "archived": False}, {"id": "hc-ab12cd34", "name": "HC-Ab12cd34", "archived": False}])
    found = find_thread_by_title(stub, "HC-Ab12cd34")  # type: ignore[arg-type]
    assert found is not None and found["id"] == "hc-ab12cd34", found
    assert stub.calls == [("GET", "/api/magician/v2/ui-threads", {"q": "HC-Ab12cd34", "limit": THREAD_LIST_PAGE_LIMIT})], stub.calls
    assert find_thread_by_title(StubThreads([]), "HC-Ab12cd34") is None  # type: ignore[arg-type]
    assert find_thread_by_title(StubThreads([{"id": "other", "name": "Other"}]), "HC-Ab12cd34") is None  # type: ignore[arg-type]
    # Verdict rule, pinned.
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False)[0] == "pass"
    assert decide_verdict(effect_ok=True, proof_ok=False, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False)[0] == "fail"
    assert decide_verdict(effect_ok=False, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False)[0] == "fail"
    assert decide_verdict(effect_ok=False, proof_ok=True, baseline=False, partial_on_effect_miss=True, cli_available=True, inconclusive=False)[0] == "partial"
    assert decide_verdict(effect_ok=True, proof_ok=False, baseline=True, partial_on_effect_miss=False, cli_available=True, inconclusive=False)[0] == "fail"
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=False, inconclusive=False)[0] == "cli_unavailable"
    assert decide_verdict(effect_ok=None, proof_ok=None, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=True)[0] == "inconclusive"
    assert decide_verdict(effect_ok=True, proof_ok=None, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False)[0] == "partial"
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, tools_expected=True, hands_attributed=False)[0] == "partial"
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, tools_expected=True, hands_attributed=True)[0] == "pass"
    # The no-tools oracle: hands on the forbidden turn fail the case even with
    # the effect present and the proof true; unset or honoured leaves the rule alone.
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, tools_expected=True, hands_attributed=True, no_tools_ok=False) == ("fail", "tools used on the no-tools turn")
    assert decide_verdict(effect_ok=False, proof_ok=True, baseline=False, partial_on_effect_miss=True, cli_available=True, inconclusive=False, no_tools_ok=False)[0] == "fail"
    for honoured in (None, True):
        assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, tools_expected=True, hands_attributed=True, no_tools_ok=honoured)[0] == "pass"
        assert decide_verdict(effect_ok=False, proof_ok=True, baseline=False, partial_on_effect_miss=True, cli_available=True, inconclusive=False, no_tools_ok=honoured)[0] == "partial"
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=False, inconclusive=False, no_tools_ok=False)[0] == "cli_unavailable"
    quiet = TurnRecord(2, "t2", "p", "a", "harness", None, 0.0, 1, 1, 1.0, True, [])
    handsy = TurnRecord(2, "t2", "p", "a", "harness", None, 0.0, 1, 1, 1.0, True, ["get_task_details"])
    first = TurnRecord(1, "t1", "p", "a", "harness", None, 0.0, 1, 1, 1.0, True, ["get_task_details"])
    assert no_tools_check({"no_tools_turn": 2}, [first, quiet]) == (True, [])
    assert no_tools_check({"no_tools_turn": 2}, [first, handsy]) == (False, ["get_task_details"])
    assert no_tools_check({}, [first, handsy]) == (None, [])
    assert no_tools_check({"no_tools_turn": 2}, [first]) == (None, [])
    # The streaming oracle: too few token frames fail the case even with the
    # effect present and the proof true; an exempt engine grades partial with
    # the documented reason; an unset or met bound leaves the rule alone, and
    # the effect and proof still come first.
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, stream_ok=False) == ("fail", "too few streamed token frames")
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, stream_ok=False, stream_exempt=True) == ("partial", "engine cannot stream (documented)")
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, stream_ok=True, stream_exempt=True)[0] == "pass"
    for unset_or_met in (None, True):
        assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, stream_ok=unset_or_met)[0] == "pass"
    assert decide_verdict(effect_ok=False, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, stream_ok=False, stream_exempt=True) == ("fail", "effect missing")
    assert decide_verdict(effect_ok=True, proof_ok=False, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, stream_ok=False)[1].startswith("fallback")
    assert decide_verdict(effect_ok=True, proof_ok=None, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, stream_ok=False)[0] == "fail"
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=False, inconclusive=False, stream_ok=False)[0] == "cli_unavailable"
    one_frame = TurnRecord(1, "t1", "p", "a", "harness", None, 0.0, 1, 1, 1.0, True, [], token_events=1)
    many_frames = TurnRecord(1, "t1", "p", "a", "harness", None, 0.0, 1, 1, 1.0, True, [], token_events=7)
    stream_case = {"stream": True, "min_token_events": 2, "engines_exempt": ["codex"]}
    assert stream_check(stream_case, [one_frame]) == (False, "turn 1 streamed 1 token frame(s), wanted at least 2")
    assert stream_check(stream_case, [many_frames]) == (True, "")
    assert stream_check(stream_case, []) == (None, "")
    assert stream_check({"min_token_events": 2}, [one_frame]) == (None, "")
    assert stream_check({}, [one_frame]) == (None, "")
    assert stream_exempt(stream_case, "codex") and not stream_exempt(stream_case, "grok") and not stream_exempt({}, "codex")
    # The leak oracle: the withheld value in the named turn's answer fails the
    # case ahead of the effect; unset, unrendered or unrun leaves the rule alone.
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, no_leak_ok=False) == ("fail", "the withheld value leaked into an earlier answer")
    assert decide_verdict(effect_ok=False, proof_ok=True, baseline=False, partial_on_effect_miss=True, cli_available=True, inconclusive=False, no_leak_ok=False)[0] == "fail"
    assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, no_tools_ok=False, no_leak_ok=False)[1] == "tools used on the no-tools turn"
    for kept in (None, True):
        assert decide_verdict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False, no_leak_ok=kept)[0] == "pass"
    withhold_case = {"turn_answer_must_not_contain": {"turn": 1, "value": "{secret}"}}
    tight = TurnRecord(1, "t1", "p", "DONE", "harness", None, 0.0, 1, 1, 1.0, True, ["get_task_details"])
    loose = TurnRecord(1, "t1", "p", "The secret is Kumquat-Ab12.", "harness", None, 0.0, 1, 1, 1.0, True, ["get_task_details"])
    leak_result = CaseResult("chat", "grok", "x", 1, "fail", "", True, True, "", [tight], {"withheld_value": "kumquat-ab12"})
    assert leak_check(withhold_case, leak_result) == (True, "")
    leak_result.turns = [loose]
    assert leak_check(withhold_case, leak_result) == (False, "turn 1 answered with 'kumquat-ab12'")
    assert leak_check({}, leak_result) == (None, "")
    assert leak_check(withhold_case, CaseResult("chat", "grok", "x", 1, "fail", "", True, True, "", [loose], {})) == (None, "")
    assert leak_check(withhold_case, CaseResult("chat", "grok", "x", 1, "fail", "", True, True, "", [], {"withheld_value": "k"})) == (None, "")
    # The one grader, live and regrade alike: the exact reasons it writes.
    def graded(case: dict[str, Any], engine: str, turns: list[TurnRecord], artifacts: dict[str, Any], *, effect_ok: bool | None = True, proof_ok: bool | None = True) -> tuple[str, str]:
        result = CaseResult("chat", engine, "x", 1, "fail", "", effect_ok, proof_ok, "", list(turns), dict(artifacts))
        grade_case_result(case, engine, engine == "magician", result)
        return result.verdict, result.reason
    assert graded({"tools_expected": True}, "grok", [tight], {}) == ("pass", "effect present and the expected author did the work")
    assert graded({"no_tools_turn": 2}, "grok", [first, handsy], {}) == ("fail", "tools used on the no-tools turn: get_task_details")
    assert graded({**withhold_case, "tools_expected": True}, "grok", [loose], {"withheld_value": "kumquat-ab12"}) == ("fail", "the withheld value leaked into an earlier answer: turn 1 answered with 'kumquat-ab12'")
    assert graded(stream_case, "grok", [one_frame], {}) == ("fail", "too few streamed token frames: turn 1 streamed 1 token frame(s), wanted at least 2")
    assert graded(stream_case, "codex", [one_frame], {}) == ("partial", "engine cannot stream (documented): turn 1 streamed 1 token frame(s), wanted at least 2")
    assert graded(stream_case, "codex", [many_frames], {}) == ("pass", "effect present and the expected author did the work")
    assert graded({"partial_on_effect_miss": True, "partial_reason": "writes can be deferred"}, "grok", [tight], {}, effect_ok=False) == ("partial", "effect missing; fixture marks this as a deferrable effect: writes can be deferred")
    assert graded({"tools_expected": True}, "grok", [quiet], {}) == ("partial", "effect present but no tool-lineage fact attributes the hands to this turn")
    assert graded({}, "magician", [TurnRecord(1, "t0", "p", "a", "harness", None, 0.0, 1, 1, 1.0, True, [])], {}, proof_ok=False) == ("fail", "baseline turn was answered by a harness instead of Magician")
    graded_result = CaseResult("chat", "grok", "x", 1, "fail", "", True, True, "", [loose, handsy], {"withheld_value": "kumquat-ab12"})
    grade_case_result({**withhold_case, "no_tools_turn": 2}, "grok", False, graded_result)
    assert (graded_result.no_tools_ok, graded_result.no_leak_ok, graded_result.stream_ok, graded_result.artifacts["hands"]) == (False, False, None, ["get_task_details"])
    # The bounded reader: terminated lines pass through, an unterminated one
    # is still counted against the bound.
    assert list(bounded_lines(io.BytesIO(b"a\nb\n"), "p")) == [b"a\n", b"b\n"]
    try:
        list(bounded_lines(io.BytesIO(b"x" * (MAX_RESPONSE_BYTES + 1)), "p"))
    except EvalFailure:
        pass
    else:
        raise AssertionError("bounded_lines accepted an unterminated line past the bound")
    # The SSE reader on a canned byte stream: frames split across lines and
    # chunks, a comment, a multi-line data field, and a trailing `done`
    # carrying the synchronous response shape.
    canned = (
        b": keep-alive\n"
        b"event: token\ndata: {\"text\": \"Hel\"}\n\n"
        b"event: token\r\ndata: {\"text\": \"lo\"}\r\n\r\n"
        b"event: reasoning_delta\ndata: {\"index\": 0, \"delta\": \"x\"}\n\n"
        b"event: done\ndata: {\"assistant_message\": {\"content\": {\"text\": \"Hello\"}},\n"
        b"data:  \"usage\": {\"provider\": \"harness\"}}\n\n"
    )
    frames = list(parse_sse(canned.splitlines(keepends=True)))
    assert [f.event for f in frames] == ["token", "token", "reasoning_delta", "done"], frames
    assert frames[-1].data == '{"assistant_message": {"content": {"text": "Hello"}},\n "usage": {"provider": "harness"}}', frames[-1]
    folded = fold_stream(frames)
    assert (folded.answer, folded.token_events, folded.errors, folded.failed()) == ("Hello", 2, [], False), folded
    assert folded.done["usage"]["provider"] == "harness", folded
    # Without an assistant message on `done`, the tokens are the answer; an
    # `error` frame is carried back for the caller to record, and with no
    # answer behind it the turn has failed.
    folded = fold_stream(parse_sse(
        b"event: token\ndata: {\"text\": \"par\"}\n\nevent: token\ndata: {\"text\": \"tial\"}\n\n"
        b"event: error\ndata: {\"error\": \"boom\"}\n\nevent: done\ndata: {}\n\n".splitlines(keepends=True)
    ))
    assert (folded.answer, folded.token_events, folded.done, folded.errors, folded.failed()) == ("partial", 2, {}, ["boom"], True), folded
    folded = fold_stream(parse_sse(
        b"event: error\ndata: {\"error\": \"late\"}\n\nevent: done\ndata: {\"assistant_message\": {\"content\": {\"text\": \"ok\"}}}\n\n".splitlines(keepends=True)
    ))
    assert (folded.answer, folded.errors, folded.failed()) == ("ok", ["late"], False), folded
    assert list(parse_sse([b"event: done\n", b"data: {}"])) == [SseFrame("done", "{}")]
    assert list(parse_sse([])) == []
    # Event shapes: the top-level wrapper and the flat row.
    seen, tools = harness_proof_from_events([
        {"event": {"event_type": "llm.succeeded", "payload": {"provider": "harness"}}},
        {"event_type": "tool.call.started", "payload": {"tool_name": "create_task"}},
    ])
    assert seen and tools == ["create_task"], (seen, tools)
    # The shape the per-turn endpoint actually serves: the sink's tagged
    # envelope with the agent event under `data.event`. Rows of other transport
    # variants carry no agent event and must be skipped, not misread.
    seen, tools = harness_proof_from_events([
        {"event_type": "ChatMessageReceived", "data": {"session_id": "s", "message": {"direction": "user"}}},
        {"event_type": "AgentEvent", "data": {"event": {
            "event_type": "tool.call.started", "agent_id": "a", "principal": "p", "workspace": "w",
            "payload": {"chat_turn_id": "t", "call_id": "c", "tool_name": "update_task", "args": {}},
            "timestamp": 1,
        }}},
        {"event_type": "AgentEvent", "data": {"event": {
            "event_type": "tool.call.finished", "principal": "p", "workspace": "w",
            "payload": {"chat_turn_id": "t", "call_id": "c", "tool_name": "update_task", "ok": True},
        }}},
    ])
    assert tools == ["update_task"] and not seen, (seen, tools)
    seen, tools = harness_proof_from_events([
        {"event_type": "AgentEvent", "data": {"event": {
            "event_type": "llm.succeeded", "payload": {"chat_turn_id": "t", "provider": "harness"},
        }}},
    ])
    assert seen and tools == [], (seen, tools)
    assert render("Create `{title}` ({nonce})", {"nonce": "abcd", "title": "HC-abcd"}) == "Create `HC-abcd` (abcd)"
    # Report shape with a synthetic matrix.
    turn = TurnRecord(1, "t1", "p", "PONG", "harness", None, 0.0, 2, 5, 900.0, True, [])
    results = [
        CaseResult("chat", "magician", "pong", 1, "pass", "ok", True, True, "d", [TurnRecord(1, "t0", "p", "PONG", "openai", "m", 0.003, 10, 6, 1200.0, False, [])], {}, 1200.0),
        CaseResult("chat", "claude_code", "pong", 1, "pass", "ok", True, True, "d", [turn], {}, 900.0),
        CaseResult("chat", "grok", "pong", 1, "cli_unavailable", "grok not on PATH", None, None, "", [], {}, 0.0),
    ]
    summaries = [
        EngineSummary("magician", True, "built-in", {"pass": 1, "partial": 0, "fail": 0, "cli_unavailable": 0, "inconclusive": 0}, 1200.0),
        EngineSummary("claude_code", True, "answered", {"pass": 1, "partial": 0, "fail": 0, "cli_unavailable": 0, "inconclusive": 0}, 900.0),
        EngineSummary("grok", True, "grok not on PATH", {"pass": 0, "partial": 0, "fail": 0, "cli_unavailable": 1, "inconclusive": 0}, 0.0),
    ]
    payload = report_payload("chat", "self-test", results, summaries, {"saved_engine": "magician", "saved_model": "default", "restore_error": None, "interrupted": False, "roster": {}})
    assert payload["summary"]["ok"] is True
    # A report written before the no-tools, streaming, leak and trace fields existed still regrades.
    later_fields = ("no_tools_ok", "stream_ok", "no_leak_ok", "trace_ok", "trace_followed")
    older = {k: v for k, v in payload["results"][0].items() if k not in later_fields}
    older["turns"] = [{k: v for k, v in t.items() if k != "token_events"} for t in older["turns"]]
    rebuilt = CaseResult(**{**older, "turns": [TurnRecord(**t) for t in older["turns"]]})
    assert all(getattr(rebuilt, k) is None for k in later_fields) and rebuilt.turns[0].token_events == 0
    assert all(payload["results"][0][k] is None for k in later_fields)
    assert payload["summary"]["engines"][2]["line"].endswith("1 cli_unavailable")
    write_report(args.output_dir, payload)
    assert (args.output_dir / "report.json").exists() and (args.output_dir / "report.html").exists()
    print(f"self-test passed; report at {args.output_dir / 'report.html'}")
    return 0


class StubRunClient:
    """A canned runtime for the run lane: one probe task, one run whose status
    reads come from a script, the journal the proof reads and the facts the
    trace reads. Records every call so the self-test can assert the lane's
    order of operations, not only its verdict."""

    def __init__(
        self,
        *,
        statuses: list[str],
        events: list[dict[str, Any]],
        task_rows: list[dict[str, Any]],
        window_rows: list[dict[str, Any]] | None = None,
        title_suffix: str = "",
        facts_error: bool = False,
    ):
        self.statuses = list(statuses)
        self.events = events
        self.task_rows = task_rows
        self.window_rows = window_rows if window_rows is not None else list(task_rows)
        # What the task's title reads after the run: the title the lane
        # created (it carries the case's nonce) plus this suffix.
        self.title_suffix = title_suffix
        self.title = ""
        self.facts_error = facts_error
        self.calls: list[tuple[str, str]] = []

    def json(self, method: str, path: str, body: Any = None, query: Any = None, expected: Any = (200,), timeout: Any = None) -> tuple[Any, float]:
        self.calls.append((method, path))
        if method == "POST" and path == "/api/magician/v3/tasks":
            self.title = body["title"]
            return {"task": {"manifest": {"task_id": "task_1", "title": self.title}}}, 1.0
        if method == "POST" and path.endswith("/execute"):
            return {"task": {}, "execution": {"state": {"execution_id": "exec_1", "status": "running"}, "refs": {}}}, 1.0
        if method == "GET" and "/executions/" in path:
            status = self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]
            return {"state": {"execution_id": "exec_1", "status": status}, "refs": {}}, 1.0
        if method == "GET" and path.startswith("/api/magician/v3/tasks/"):
            return {"task": {"manifest": {"task_id": "task_1", "title": self.title + self.title_suffix}, "state": {}}}, 1.0
        if method == "POST" and path.endswith("/cancel"):
            return {"cancelled": True}, 1.0
        if method == "POST" and path.endswith("/facts/query"):
            if self.facts_error:
                raise EvalFailure("POST facts/query -> HTTP 400: relation refused")
            rows = self.task_rows if "WHERE task_id" in body["sql"] else self.window_rows
            return {"data": {"rows": rows, "row_count": len(rows)}}, 1.0
        if method == "DELETE":
            return {"ok": True}, 1.0
        raise AssertionError(f"unexpected call {method} {path}")

    def request(self, method: str, path: str, body: Any = None, query: Any = None, timeout: Any = None) -> tuple[int, bytes, float]:
        self.calls.append((method, path))
        assert method == "GET" and path == "/api/magician/v3/events" and query["backfill_only"] == "true", (method, path, query)
        return 200, b"\n" + "\n".join(json.dumps(e) for e in self.events).encode("utf-8") + b"\n", 1.0


def self_test_run(args: argparse.Namespace) -> int:
    cases = load_cases(FIXTURE_DIR / "run_cases.json", "run")
    assert [c["id"] for c in cases] == ["run_task_update", "run_parent_follow"], cases
    update, follow = cases
    # The same run twice: one graded by effect and proof, one by the trace too.
    assert update["description"] == follow["description"] and "HC-{nonce}-run-updated" in update["description"]
    assert update["effect"] == follow["effect"] == {"kind": "task_field_contains", "field": "title", "value": "HC-{nonce}-run-updated"}
    assert "trace" not in update and follow["trace"] is True, (update, follow)
    bad_fixture = args.output_dir / "self-test-bad-run-cases.json"
    args.output_dir.mkdir(parents=True, exist_ok=True)
    good = {"id": "x", "description": "d", "effect": {"kind": "task_field_contains", "field": "title", "value": "v"}}
    for bad in (
        {"description": ""},
        {"description": 3},
        {"effect": {"kind": "answer_contains", "value": "v"}},
        {"effect": {"kind": "task_field_contains", "field": "", "value": "v"}},
        {"effect": {"kind": "task_field_contains", "field": "title"}},
        {"trace": "yes"},
        {"turns": ["a"]},
        {"stream": True},
    ):
        bad_fixture.write_text(json.dumps({"lane": "run", "cases": [{**good, **bad}]}), encoding="utf-8")
        try:
            load_cases(bad_fixture, "run")
        except EvalFailure:
            pass
        else:
            raise AssertionError(f"load_cases accepted the run case {bad}")
    bad_fixture.write_text(json.dumps({"lane": "run", "cases": [{**good, "trace": False, "notes": "n"}]}), encoding="utf-8")
    assert load_cases(bad_fixture, "run")[0]["trace"] is False
    bad_fixture.write_text(json.dumps({"lane": "chat", "cases": [good]}), encoding="utf-8")
    try:
        load_cases(bad_fixture, "run")
    except EvalFailure:
        pass
    else:
        raise AssertionError("load_cases accepted a chat fixture for the run lane")
    bad_fixture.unlink()
    # Engine -> provider family: the two renamed engines and the baseline.
    assert harness_provider_for("claude_code") == "harness-claude_code" and harness_provider_for("codex_app_server") == "harness-codex"
    assert harness_provider_for("grok") == "harness-grok" and harness_provider_for("agy") == "harness-agy"
    assert harness_provider_for("magician") is None
    assert is_tool_carrying("agentic_decision") and is_tool_carrying("agentic_decision:retry") and not is_tool_carrying("task_summary")
    # The proof off the journal: canonical rows as the events route serves
    # them, flat with `event_type` and `payload`.
    def settled(engine: str) -> dict[str, Any]:
        return {"event_type": "execution.progress", "payload": {"kind": "harness_turn_settled", "engine": engine, "iteration": 1}}

    native = {"event_type": "llm.succeeded", "payload": {"operation": "agentic_decision", "provider": "openai"}}
    hand = {"event_type": "tool.succeeded", "payload": {"target": "pack:update_task(task_id=\"t\")", "iteration": 1}}
    proof = read_run_proof([settled("codex"), settled("codex"), hand])
    assert (proof.events, proof.harness_engines, proof.native_decisions, proof.hands) == (3, ["codex", "codex"], 0, ["pack:update_task"]), proof
    assert run_engine_proof(proof, "codex") == (True, "2 harness turn(s) settled on codex")
    assert run_engine_proof(proof, "grok")[0] is False
    assert run_engine_proof(proof, "magician")[0] is False
    assert run_engine_proof(read_run_proof([native, hand]), "magician") == (True, "1 native decision call(s), no harness turn")
    assert run_engine_proof(read_run_proof([native, hand]), "codex")[0] is False
    assert run_engine_proof(read_run_proof([hand]), "magician")[0] is None
    assert run_engine_proof(read_run_proof([]), "codex")[0] is None
    # The trace: operations and their providers, the floor and the rule.
    decision_api = {"operation": "agentic_decision", "provider": "openai", "timestamp_ms": 2}
    decision_harness = {"operation": "agentic_decision", "provider": "harness-codex", "timestamp_ms": 2}
    summary_parent = {"operation": "task_summary", "provider": "harness-codex", "timestamp_ms": 3}
    summary_api = {"operation": "task_summary", "provider": "openai", "timestamp_ms": 3}
    summary_local = {"operation": "ledger_compaction", "provider": "ollama", "timestamp_ms": 4}
    good_trace = trace_check([decision_api, summary_parent, summary_local], "codex")
    assert (good_trace.ok, good_trace.followed, good_trace.text_ops) == (True, True, 2), good_trace
    assert good_trace.seen == {"agentic_decision": ["openai"], "ledger_compaction": ["ollama"], "task_summary": ["harness-codex"]}
    rode = trace_check([decision_harness, summary_parent], "codex")
    assert (rode.ok, rode.followed) == (False, True) and "['agentic_decision'] rode a harness provider" in rode.detail, rode
    behind = trace_check([decision_api, summary_api], "codex")
    assert (behind.ok, behind.followed, behind.text_ops) == (True, False, 1), behind
    only_tools = trace_check([decision_api], "codex")
    assert (only_tools.ok, only_tools.followed, only_tools.text_ops) == (True, False, 0), only_tools
    assert trace_check([], "codex").ok is None and trace_check([], "codex").followed is None
    clean_baseline = trace_check([decision_api, summary_api], "magician")
    assert (clean_baseline.ok, clean_baseline.followed) == (True, None), clean_baseline
    stray_baseline = trace_check([decision_api, summary_parent], "magician")
    assert (stray_baseline.ok, stray_baseline.followed) == (False, None) and "['task_summary']" in stray_baseline.detail, stray_baseline
    # The verdict rule with the trace parameters: the floor fails, no rows is
    # inconclusive, the rule partial; effect and proof still come first, and
    # an ungraded trace leaves the rule alone.
    base = dict(effect_ok=True, proof_ok=True, baseline=False, partial_on_effect_miss=False, cli_available=True, inconclusive=False)
    assert decide_verdict(**base, trace_rows=3, trace_ok=True, trace_followed=True) == ("pass", "effect present and the expected author did the work")
    assert decide_verdict(**base, trace_rows=3, trace_ok=False, trace_followed=True) == ("fail", TRACE_TOOL_CALL_RODE_HARNESS)
    assert decide_verdict(**{**base, "baseline": True}, trace_rows=3, trace_ok=False) == ("fail", TRACE_BASELINE_RODE_HARNESS)
    assert decide_verdict(**base, trace_rows=0) == ("inconclusive", TRACE_NO_ANALYTICS_ROWS)
    assert decide_verdict(**base, trace_rows=3, trace_ok=True, trace_followed=False) == ("partial", TRACE_NO_TEXT_OP_ON_PARENT)
    assert decide_verdict(**{**base, "effect_ok": False}, trace_rows=3, trace_ok=False) == ("fail", "effect missing")
    assert decide_verdict(**{**base, "proof_ok": False}, trace_rows=0)[1].startswith("fallback")
    assert decide_verdict(**{**base, "proof_ok": None}, trace_rows=0) == ("inconclusive", TRACE_NO_ANALYTICS_ROWS)
    assert decide_verdict(**{**base, "proof_ok": None}, trace_rows=3, trace_ok=True, trace_followed=True)[0] == "partial"
    assert decide_verdict(**{**base, "cli_available": False}, trace_rows=3, trace_ok=False)[0] == "cli_unavailable"
    assert decide_verdict(**base)[0] == "pass" and decide_verdict(**base, trace_rows=None, trace_ok=None, trace_followed=None)[0] == "pass"
    # The one grader, on canned results: the exact reasons it writes.
    def graded(case: dict[str, Any], engine: str, artifacts: dict[str, Any], *, effect_ok: bool | None = True, proof_ok: bool | None = True) -> tuple[str, str, bool | None, bool | None]:
        result = CaseResult("run", engine, case.get("id", "x"), 1, "fail", "", effect_ok, proof_ok, "", [], {"run_outcome": "terminal", "run_status": "completed", **artifacts})
        grade_run_result(case, engine, engine == "magician", result)
        return result.verdict, result.reason, result.trace_ok, result.trace_followed
    traced = {"trace": True}
    assert graded(traced, "codex", {"analytics_rows": [decision_api, summary_parent]}) == ("pass", "effect present and the expected author did the work", True, True)
    verdict, reason, ok, followed = graded(traced, "codex", {"analytics_rows": [decision_harness, summary_parent]})
    assert (verdict, ok, followed) == ("fail", False, True) and reason.startswith("tool_call_rode_harness: ") and "seen agentic_decision=harness-codex, task_summary=harness-codex" in reason, reason
    verdict, reason, ok, followed = graded(traced, "codex", {"analytics_rows": [decision_api, summary_api]})
    assert (verdict, ok, followed) == ("partial", True, False) and reason.startswith("no_text_op_on_parent: ") and "none rode harness-codex" in reason, reason
    verdict, reason, ok, followed = graded(traced, "codex", {"analytics_rows": [decision_api]})
    assert (verdict, ok, followed) == ("partial", True, False) and reason.startswith("no_text_op_observed: ") and "seen agentic_decision=openai" in reason, reason
    verdict, reason, ok, followed = graded(traced, "codex", {"analytics_rows": []})
    assert (verdict, reason, ok, followed) == ("inconclusive", TRACE_NO_ANALYTICS_ROWS, None, None), reason
    verdict, reason, _, _ = graded(traced, "codex", {"analytics_rows": [], "analytics_error": "HTTP 400"})
    assert verdict == "inconclusive" and reason.endswith(": HTTP 400"), reason
    verdict, reason, ok, followed = graded(traced, "magician", {"analytics_rows": [decision_api, summary_parent]})
    assert (verdict, ok, followed) == ("fail", False, None) and reason.startswith("baseline_op_rode_harness: ") and "['task_summary'] rode" in reason, reason
    assert graded(traced, "magician", {"analytics_rows": [decision_api, summary_api]}) == ("pass", "effect present and the expected author did the work", True, None)
    # A window row on a harness provider the task rows lack is named on a partial.
    verdict, reason, _, _ = graded(traced, "codex", {"analytics_rows": [decision_api, summary_api], "window_harness_providers": ["harness-codex"]})
    assert verdict == "partial" and reason.endswith("the window holds ['harness-codex'] outside the task's trace context"), reason
    # The untraced case ignores the rows; effect and proof still rule it.
    assert graded({}, "codex", {"analytics_rows": [decision_harness]}) == ("pass", "effect present and the expected author did the work", None, None)
    assert graded({}, "codex", {"analytics_rows": []}) == ("pass", "effect present and the expected author did the work", None, None)
    assert graded({}, "codex", {}, effect_ok=False)[0:2] == ("fail", "effect missing")
    assert graded({}, "codex", {}, proof_ok=False)[1].startswith("fallback")
    assert graded({}, "magician", {}, proof_ok=False) == ("fail", "baseline turn was answered by a harness instead of Magician", None, None)
    # A run that stopped for a person or ran out the deadline is decided by
    # that, whatever its task now says.
    verdict, reason, _, _ = graded(traced, "codex", {"run_outcome": "waiting", "run_status": "waiting_for_user", "analytics_rows": [summary_parent]})
    assert verdict == "fail" and reason.startswith("run_waited_for_input: ") and "'waiting_for_user'" in reason, reason
    verdict, reason, _, _ = graded(traced, "codex", {"run_outcome": "timeout", "run_status": "running", "run_deadline_secs": 900.0})
    assert verdict == "inconclusive" and reason.startswith("run_timeout: ") and "900.0s deadline" in reason, reason
    # The run itself against the canned runtime: the order of calls, the
    # effect off the task, the proof off the journal, the trace off the facts,
    # and the cleanup.
    follow_case = {**follow}
    stub = StubRunClient(statuses=["running", "completed"], events=[settled("codex"), hand], task_rows=[summary_parent, summary_local], title_suffix="-run-updated")
    result = run_run_case(stub, engine="codex", baseline=False, case=follow_case, run=1, run_timeout=5.0, keep_artifacts=False, poll_seconds=0.0, analytics_deadline=0.0)  # type: ignore[arg-type]
    assert (result.verdict, result.effect_ok, result.proof_ok, result.trace_ok, result.trace_followed) == ("pass", True, True, True, True), (result.verdict, result.reason)
    assert result.artifacts["run_status"] == "completed" and result.artifacts["run_outcome"] == "terminal"
    assert result.artifacts["harness_engines_seen"] == ["codex"] and result.artifacts["hands"] == ["pack:update_task"]
    assert result.artifacts["operations_seen"] == {"ledger_compaction": ["ollama"], "task_summary": ["harness-codex"]}
    assert result.artifacts["providers_seen"] == ["harness-codex", "ollama"] and result.artifacts["task_deleted"] is True
    assert result.artifacts["analytics_sql"] == "SELECT operation, profile, provider, model, success, execution_id, timestamp_ms FROM llm_calls WHERE task_id = 'task_1'"
    assert "proof: 1 harness turn(s) settled on codex" in result.effect_detail and "trace: ledger_compaction=ollama, task_summary=harness-codex" in result.effect_detail
    methods = [(m, p.rsplit("/", 1)[-1]) for m, p in stub.calls]
    assert methods[:3] == [("POST", "tasks"), ("POST", "execute"), ("GET", "exec_1")] and ("POST", "cancel") not in methods and methods[-1] == ("DELETE", "task_1"), methods
    assert ("GET", "events") in methods and methods.count(("POST", "query")) == 2
    # A run that waits for a person is cancelled, read once, deleted, failed.
    stub = StubRunClient(statuses=["running", "waiting_for_user", "waiting_for_user"], events=[settled("codex")], task_rows=[])
    result = run_run_case(stub, engine="codex", baseline=False, case=follow_case, run=1, run_timeout=5.0, keep_artifacts=False, poll_seconds=0.0, analytics_deadline=0.0)  # type: ignore[arg-type]
    assert result.verdict == "fail" and result.reason.startswith("run_waited_for_input") and result.artifacts["run_cancelled"] is True, (result.verdict, result.reason)
    assert result.effect_ok is False and result.artifacts["task_deleted"] is True
    methods = [(m, p.rsplit("/", 1)[-1]) for m, p in stub.calls]
    assert ("POST", "cancel") in methods and methods.index(("POST", "cancel")) < methods.index(("DELETE", "task_1")), methods
    # One waiting read between running reads is not a stop.
    stub = StubRunClient(statuses=["paused", "running", "completed"], events=[settled("codex")], task_rows=[summary_parent], title_suffix="-run-updated")
    result = run_run_case(stub, engine="codex", baseline=False, case=follow_case, run=1, run_timeout=5.0, keep_artifacts=False, poll_seconds=0.0, analytics_deadline=0.0)  # type: ignore[arg-type]
    assert result.verdict == "pass" and "run_cancelled" not in result.artifacts, (result.verdict, result.reason)
    # A run past the deadline is inconclusive and cancelled.
    stub = StubRunClient(statuses=["running"], events=[], task_rows=[])
    result = run_run_case(stub, engine="codex", baseline=False, case=follow_case, run=1, run_timeout=0.0, keep_artifacts=True, poll_seconds=0.0, analytics_deadline=0.0)  # type: ignore[arg-type]
    assert result.verdict == "inconclusive" and result.reason.startswith("run_timeout") and result.artifacts["run_cancelled"] is True, (result.verdict, result.reason)
    assert "task_deleted" not in result.artifacts and ("DELETE", "task_1") not in [(m, p.rsplit("/", 1)[-1]) for m, p in stub.calls]
    # The baseline through the same path: native decisions, no harness row.
    stub = StubRunClient(statuses=["completed"], events=[native, hand], task_rows=[decision_api, summary_api], title_suffix="-run-updated")
    result = run_run_case(stub, engine="magician", baseline=True, case=follow_case, run=1, run_timeout=5.0, keep_artifacts=False, poll_seconds=0.0, analytics_deadline=0.0)  # type: ignore[arg-type]
    assert (result.verdict, result.proof_ok, result.trace_ok, result.trace_followed) == ("pass", True, True, None), (result.verdict, result.reason)
    # A refused fact query is recorded, not mistaken for an empty trace.
    stub = StubRunClient(statuses=["completed"], events=[settled("codex")], task_rows=[], title_suffix="-run-updated", facts_error=True)
    result = run_run_case(stub, engine="codex", baseline=False, case=follow_case, run=1, run_timeout=5.0, keep_artifacts=False, poll_seconds=0.0, analytics_deadline=0.0)  # type: ignore[arg-type]
    assert result.verdict == "inconclusive" and result.reason.startswith(TRACE_NO_ANALYTICS_ROWS) and "relation refused" in result.reason, result.reason
    # The engine switch: the saved model rides the restore, since the PUT
    # would otherwise reset it.
    class StubEngines:
        def __init__(self) -> None:
            self.puts: list[dict[str, Any]] = []

        def json(self, method: str, path: str, body: Any = None, **_: Any) -> tuple[Any, float]:
            if method == "GET":
                return {"engines": [{"name": "codex", "installed": True}, {"name": "grok", "installed": False}, {"name": "magician", "installed": True}], "current": "claude_code", "run_model": "opus", "chat_current": "magician"}, 1.0
            assert method == "PUT" and path == "/api/magician/v2/plane/engine", (method, path)
            self.puts.append(body)
            return {"engine": body["harness_engine"], "harness_model": body.get("harness_model", "default")}, 1.0

    engines_stub = StubEngines()
    switch = RunEngineSwitch(engines_stub)  # type: ignore[arg-type]
    roster, engines = roster_engines(switch.save(), None)
    assert (switch.saved_engine, switch.saved_model) == ("claude_code", "opus") and engines == ["magician", "codex"] and roster["grok"] is False
    switch.select("codex")
    switch.restore()
    assert engines_stub.puts == [{"harness_engine": "codex"}, {"harness_engine": "claude_code", "harness_model": "opus"}], engines_stub.puts
    assert roster_engines({"engines": [{"name": "codex", "installed": True}]}, ["codex"])[1] == ["codex"]
    try:
        roster_engines({"engines": [{"name": "codex", "installed": True}]}, ["agy"])
    except EvalFailure:
        pass
    else:
        raise AssertionError("roster_engines accepted an engine off the roster")
    assert [c["id"] for c in select_cases(cases, ["run_parent_follow"])] == ["run_parent_follow"]
    try:
        select_cases(cases, ["nope"])
    except EvalFailure:
        pass
    else:
        raise AssertionError("select_cases accepted an unknown case id")
    assert [r.verdict for r in cli_unavailable_results("run", "grok", cases, 2, "grok not on PATH")] == ["cli_unavailable"] * 4
    # Report shape with a synthetic run matrix: the artifacts render where a
    # chat result's turns would, and a run report regrades through the run grader.
    passed = CaseResult("run", "codex", "run_parent_follow", 1, "pass", "ok", True, True, "d", [], {"run_outcome": "terminal", "run_status": "completed", "providers_seen": ["harness-codex", "openai"], "hands": ["pack:update_task"], "analytics_rows": [decision_api, summary_parent], "window_rows": [decision_api, summary_parent, summary_api]}, 30000.0, trace_ok=True, trace_followed=True)
    unavailable = CaseResult("run", "grok", "run_parent_follow", 1, "cli_unavailable", "grok not on PATH", None, None, "", [], {}, 0.0)
    summaries = [
        EngineSummary("codex", True, "answered", {"pass": 1, "partial": 0, "fail": 0, "cli_unavailable": 0, "inconclusive": 0}, 30000.0),
        EngineSummary("grok", False, "grok not on PATH", {"pass": 0, "partial": 0, "fail": 0, "cli_unavailable": 1, "inconclusive": 0}, 0.0),
    ]
    payload = report_payload("run", "self-test", [passed, unavailable], summaries, {"saved_engine": "magician", "saved_model": "default", "restore_error": None, "interrupted": False, "roster": {}})
    assert payload["summary"]["ok"] is True and payload["results"][0]["trace_ok"] is True
    write_report(args.output_dir, payload)
    html = (args.output_dir / "report.html").read_text(encoding="utf-8")
    assert "harness-codex, openai" in html and "pack:update_task" in html and "<summary>artifacts</summary>" in html and "3 row(s) in report.json" in html
    rebuilt = CaseResult(**{**payload["results"][0], "turns": []})
    grade_run_result(follow, rebuilt.engine, False, rebuilt)
    assert (rebuilt.verdict, rebuilt.trace_ok, rebuilt.trace_followed) == ("pass", True, True), (rebuilt.verdict, rebuilt.reason)
    print(f"self-test passed (run lane); report at {args.output_dir / 'report.html'}")
    return 0


def self_test(args: argparse.Namespace) -> int:
    """`--self-test --lane run` exercises the run lane on canned payloads;
    every other lane runs the chat self-test, which also pins the shared
    report, verdict and fixture logic."""
    if args.lane == "run":
        return self_test_run(args)
    return self_test_chat(args)


def regrade(args: argparse.Namespace) -> int:
    payload = json.loads(args.regrade.read_text(encoding="utf-8"))
    cases = {c["id"]: c for c in load_cases(args.fixtures or FIXTURE_DIR / f"{payload['lane']}_cases.json", payload["lane"])}
    results: list[CaseResult] = []
    client = Client(args.api_base_url, args.http_timeout_secs) if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip() else None
    lookback_ms = int(time.time() * 1000) - 24 * 3600 * 1000
    for raw in payload["results"]:
        turns = [TurnRecord(**t) for t in raw["turns"]]
        if client is not None:
            for turn in turns:
                for name in turn_tool_facts(client, turn.chat_turn_id, lookback_ms):
                    if name not in turn.tool_calls:
                        turn.tool_calls.append(name)
        result = CaseResult(**{**raw, "turns": turns})
        case = cases.get(result.case_id, {})
        if result.verdict in ("cli_unavailable", "inconclusive") or result.effect_ok is None and result.proof_ok is None:
            results.append(result)
            continue
        grader = grade_run_result if payload["lane"] == "run" else grade_case_result
        grader(case, result.engine, result.engine == "magician", result)
        results.append(result)
    summaries: list[EngineSummary] = []
    for engine_row in payload["summary"]["engines"]:
        counts = {v: 0 for v in VERDICTS}
        for r in results:
            if r.engine == engine_row["engine"]:
                counts[r.verdict] += 1
        summaries.append(EngineSummary(engine_row["engine"], engine_row["installed"], engine_row["cli_probe"], counts, engine_row["total_latency_ms"]))
    meta = dict(payload.get("meta") or {})
    meta["regraded_from"] = str(args.regrade)
    new_payload = report_payload(payload["lane"], payload["mode"] + " (regraded)", results, summaries, meta)
    write_report(args.output_dir, new_payload)
    for engine in new_payload["summary"]["engines"]:
        print(engine["line"])
    print(f"Harness conformance report: {args.output_dir / 'report.html'}")
    return 0


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--lane", choices=LANES, default="chat")
    parser.add_argument("--api-base-url", default=os.environ.get("HARNESS_CONFORMANCE_API_BASE_URL", DEFAULT_BASE_URL))
    parser.add_argument("--engines", type=lambda s: [e.strip() for e in s.split(",") if e.strip()], default=None,
                        help="comma-separated roster names; default = magician baseline + every installed engine")
    parser.add_argument("--case", dest="cases", action="append", help="run only this fixture case id (repeatable)")
    parser.add_argument("--execution-delegate-agent", default="simple-data-analyst",
                        help="existing permitted child agent for the execution delegate_tools fixture")
    parser.add_argument("--execution-web-research-case", default="direct_openai_sarvam_pricing",
                        help="web-researcher live-eval fixture case the execution direct_web_research case reruns per engine")
    parser.add_argument("--plane-answer-word", default=None,
                        help="the word the plane lane answers the run's question with (default: a fresh nonce word)")
    parser.add_argument("--voice-driver", choices=("auto", "text", "speech"), default="auto",
                        help="how the voice lane puts the prompt to the realtime session: typed (GPT Realtime), spoken "
                             "through say + ffmpeg (required for GPT Live 1), or auto = each profile's default")
    parser.add_argument("--fixtures", type=Path, default=None)
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--http-timeout-secs", type=float, default=60.0)
    parser.add_argument("--turn-timeout-secs", type=float, default=900.0,
                        help="per chat turn, or per run in the run lane; harness CLIs can take minutes on tool-using turns")
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--keep-artifacts", action="store_true", help="do not delete the nonce tasks and threads the cases create")
    parser.add_argument("--skip-cli-probe", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--regrade", type=Path, default=None,
                        help="re-apply the current verdict rule to an existing report.json and write a new report")
    args = parser.parse_args(argv)
    if args.runs < 1:
        parser.error("--runs must be >= 1")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.lane == "execution":
        from eval_harness_execution import main as execution_main
        return execution_main(args, Client, RuntimeUnavailable)
    if args.lane == "plane":
        from eval_harness_plane import main as plane_main
        return plane_main(args, Client, RuntimeUnavailable, probe_cli)
    if args.lane == "voice":
        from eval_harness_voice import main as voice_main
        return voice_main(args, Client, RuntimeUnavailable)
    if args.self_test:
        return self_test(args)
    if args.regrade:
        return regrade(args)
    live_lanes = {"chat": run_chat_lane, "run": run_run_lane}
    lane_runner = live_lanes.get(args.lane)
    if lane_runner is None:
        # Every lane in LANES is built and dispatched above, so this is a
        # dispatch table that fell behind its own argument parser.
        print(f"lane {args.lane!r} has no runner wired in this rig", file=sys.stderr)
        return 2
    if not os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip():
        print("MAGICIAN_BEARER_TOKEN is required for the live lane", file=sys.stderr)
        return 2
    try:
        results, summaries, meta = lane_runner(args)
    except EvalFailure as error:
        print(f"eval aborted: {error}", file=sys.stderr)
        return 2
    payload = report_payload(args.lane, "live", results, summaries, meta)
    write_report(args.output_dir, payload)
    for engine in payload["summary"]["engines"]:
        print(engine["line"])
    print(f"Harness conformance report: {args.output_dir / 'report.html'}")
    if meta.get("restore_error"):
        return 3
    return 0 if payload["summary"]["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())

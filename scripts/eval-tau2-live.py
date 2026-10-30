#!/usr/bin/env python3
"""τ²/τ³-bench (Sierra) through the real Magician chat path.

Run under tau2's own environment (Python >=3.12):

    uv run --project ~/.cache/magician-evals/tau2-bench \\
        python scripts/eval-tau2-live.py --domain retail --limit 10

tau2's runner owns the hard parts — the simulated user, the environment
database, tool EXECUTION, and the evaluator (pass^k / db+action rewards).
Magician plugs in as the agent under test through a registered
``magician_agent`` factory (``tau2.registry.register_agent_factory``):

- one fresh Magician workspace + chat session per task (isolation identical
  to the LongMemEval lane: scope-rotated login, marker file, purged after
  the task from the run's parking scope — never the engraved one; the
  task's chat transcript is exported to ``<output-dir>/transcripts/`` first,
  and ``--keep-workspaces`` skips the purge);
- the domain policy rides the session's opening message and the domain's
  tools are MOUNTED NATIVELY as skills: one governed-CLI skill per tool,
  POSTing to a driver-hosted localhost bridge that executes the call once
  against tau2's live environment. The adapter reflects those executions as
  tool calls so tau2's evaluator sees them, and a wrapped environment
  answers the orchestrator's replay from the memo (no double mutation) —
  the score measures Magician's instruction-following, policy compliance
  and tool-call correctness, nothing else;
- questions the agent asks through its input tool (HITL ``need_user_input``)
  are RELAYED to the simulated customer as the agent's message, and the
  customer's next message is posted back as the answer to the same paused
  turn (``--no-hitl-relay`` restores the canned auto-answer). Confirmations
  and choices are still auto-answered — a benchmark has no human operator.

The user simulator needs an LLM too (``--user-llm``, default from tau2);
its provider key is read from the runtime env files and exported, so the
simulator's spend shows up in the report as well as the agent's.

``--self-test`` is provider-free: the bridge, turn waiting, the HITL relay,
transcript export and the report run against fake clients, no server, no
tau2 task execution.

Live mode needs a running runtime with ``MAGICIAN_EVAL_USERNAME`` /
``MAGICIAN_EVAL_PASSWORD`` (env or ``$MAGICIAN_ROOT_DIR/.env.development``)
or ``MAGICIAN_BEARER_TOKEN``/``--bearer``.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import json
import os
import re
import secrets
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from html import escape
from pathlib import Path
from typing import Any, Callable

from chat_model_args import add_chat_model_arguments, chat_model_request_fields, resolve_chat_model_selections

print = functools.partial(print, flush=True)  # noqa: A001 - background runs must not buffer progress
REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BASE_URL = "http://127.0.0.1:3002"
DEFAULT_OUTPUT_DIR = REPO_ROOT / "coverage/evals/tau2/live/latest"
DEFAULT_LIMIT = 5
MAX_RESPONSE_BYTES = 8 * 1024 * 1024
MAX_MESSAGE_CHARS = 14_000
# A turn is judged by progress, not length: a legitimately long turn (one that
# carries a whole workflow, 40+ calls) streams events every few seconds. The
# driver gives up only when the turn's event log and the tool bridge both stay
# silent for DEFAULT_STALL_SECONDS; the wall-clock backstop exists only to
# catch a bug in that liveness signal (0 = none).
DEFAULT_STALL_SECONDS = 180.0
DEFAULT_TURN_MAX_SECONDS = 3600.0
TURN_POLL_SECONDS = 2.0
DEFAULT_MAX_COST_USD = 25.0
WORKSPACE_SLUG_PREFIX = "tau2"
WORKSPACE_MARKER_FILE = ".tau2-eval"
TRANSCRIPT_EVENTS_LIMIT = 100_000
TRANSCRIPT_MAX_MESSAGE_PAGES = 50


class EvalFailure(RuntimeError):
    pass


class RuntimeUnavailable(EvalFailure):
    """The runtime went away mid-run — inconclusive, never a wrong answer."""


class ResponseLost(RuntimeUnavailable):
    """The request was sent but its response never arrived (read timeout or a
    reset mid-response). The server may well be acting on it: a chat POST is
    held open for the whole turn, so this says nothing about the turn."""


class TurnStalled(EvalFailure):
    """The turn's event log and the tool bridge stayed silent past the stall
    window — a turn that stopped proceeding, not merely a long one."""


class TurnBackstopExceeded(EvalFailure):
    """A turn still showing progress outlived the wall-clock backstop."""


# ---------------------------------------------------------------------------
# HTTP + auth + workspaces (same contracts as scripts/eval-longmemeval-live.py)
# ---------------------------------------------------------------------------


class Client:
    def __init__(self, base_url: str, bearer: str | None):
        self.base_url = base_url.rstrip("/")
        self.bearer = bearer
        self.principal = ""

    def request(self, method: str, path: str, body: dict[str, Any] | None = None, query: dict[str, Any] | None = None, timeout: float = 120.0) -> tuple[int, bytes]:
        params = {k: v for k, v in (query or {}).items() if v is not None}
        url = f"{self.base_url}{path}"
        if params:
            url += ("&" if "?" in url else "?") + urllib.parse.urlencode(params)
        data = json.dumps(body).encode() if body is not None else None
        headers = {"Accept": "application/json", "Content-Type": "application/json"}
        if self.bearer:
            headers["Authorization"] = f"Bearer {self.bearer}"
        request = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            response = urllib.request.urlopen(request, timeout=timeout)
            status, raw = response.status, response.read(MAX_RESPONSE_BYTES + 1)
        except urllib.error.HTTPError as error:
            status, raw = error.code, error.read(MAX_RESPONSE_BYTES + 1)
        except urllib.error.URLError as error:
            # urllib wraps failures to connect or to send the request.
            raise RuntimeUnavailable(f"{method} {path}: {error}") from error
        except (TimeoutError, OSError) as error:
            # Raw socket errors surface after the request was sent, while
            # waiting for or reading the response.
            raise ResponseLost(f"{method} {path}: {error}") from error
        if len(raw) > MAX_RESPONSE_BYTES:
            raise EvalFailure(f"response exceeded {MAX_RESPONSE_BYTES} bytes: {path}")
        return status, raw

    def json(self, method: str, path: str, body: dict[str, Any] | None = None, query: dict[str, Any] | None = None, expected: tuple[int, ...] = (200,), timeout: float = 120.0) -> Any:
        status, raw = self.request(method, path, body, query, timeout=timeout)
        payload: Any = None
        if raw:
            try:
                payload = json.loads(raw)
            except json.JSONDecodeError as error:
                raise EvalFailure(f"invalid JSON from {method} {path} (HTTP {status})") from error
        if status not in expected:
            raise EvalFailure(f"{method} {path} -> HTTP {status}: {json.dumps(payload)[:400]}")
        return payload


def read_eval_credential(name: str, root: Path) -> str:
    value = os.environ.get(name, "").strip()
    if value:
        return value
    for filename in (".env.development", ".env"):
        path = root / filename
        if not path.is_file():
            continue
        for line in path.read_text().splitlines():
            if line.startswith(f"{name}="):
                candidate = line.split("=", 1)[1].strip().strip('"').strip("'")
                if candidate:
                    return candidate
    return ""


def runtime_root() -> Path:
    return Path(os.environ.get("MAGICIAN_ROOT_DIR", str(Path.home() / "MagicianNotes")))


def export_runtime_provider_keys() -> list[str]:
    """Make the runtime env files' provider keys visible to tau2's simulator."""
    exported: list[str] = []
    wanted = ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_API_KEY", "GEMINI_API_KEY", "XAI_API_KEY", "OPENROUTER_API_KEY")
    for name in wanted:
        if os.environ.get(name):
            continue
        value = read_eval_credential(name, runtime_root())
        if value:
            os.environ[name] = value
            exported.append(name)
    return exported


def login_client(args: argparse.Namespace) -> Client:
    root = runtime_root()
    username = read_eval_credential("MAGICIAN_EVAL_USERNAME", root)
    password = read_eval_credential("MAGICIAN_EVAL_PASSWORD", root)
    if not username or not password:
        raise EvalFailure(f"login needs MAGICIAN_EVAL_USERNAME/MAGICIAN_EVAL_PASSWORD (env or {root}/.env.development)")
    client = Client(args.base_url, None)
    login = client.json("POST", "/api/magician/v2/auth/login", {"username": username, "password": password}, expected=(200, 201))
    client.bearer = login["token"]
    return client


def bootstrap_client(args: argparse.Namespace) -> Client:
    bearer = args.bearer or os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
    client = Client(args.base_url, bearer) if bearer else login_client(args)
    session = client.json("GET", "/api/magician/v2/auth/session")
    client.principal = str(session.get("principal") or "")
    return client


def scope_client(client: Client, workspace: str) -> Client:
    """Rotate the session onto ``workspace`` in place; the prior token dies."""
    rotated = client.json("POST", "/api/magician/v2/auth/session/scope", {"workspace": workspace})
    client.bearer = rotated["token"]
    return client


def create_workspace(client: Client, slug: str) -> None:
    client.json(
        "POST", "/api/magician/v2/workspaces",
        {"slug": slug, "display_name": "tau2 benchmark", "description": "tau2-bench run"},
        expected=(200, 201, 409),
    )


def mark_workspace(client: Client, slug: str) -> None:
    if not client.principal:
        return
    scope_dir = runtime_root() / "scopes" / client.principal / slug
    try:
        if scope_dir.is_dir():
            (scope_dir / WORKSPACE_MARKER_FILE).write_text("Created by scripts/eval-tau2-live.py\n")
    except OSError:
        pass


def purge_workspace(client: Client, slug: str) -> None:
    try:
        client.json("DELETE", f"/api/magician/v2/workspaces/{slug}", query={"purge": "true"}, expected=(200, 202, 204))
    except EvalFailure as error:
        print(f"  warning: purge of {slug} failed: {error}")


def new_chat_session(client: Client, ui_thread_id: str, attempts: int = 3) -> str:
    last_error: Exception | None = None
    for attempt in range(attempts):
        try:
            payload = client.json("POST", "/api/magician/v2/chat/new", {}, query={"ui_thread_id": ui_thread_id})
            session = payload.get("session") if isinstance(payload, dict) else None
            session_id = (session or {}).get("id") or (payload.get("id") if isinstance(payload, dict) else None)
            if not session_id:
                raise EvalFailure(f"chat/new returned no session id: {json.dumps(payload)[:200]}")
            return str(session_id)
        except EvalFailure as error:
            message = str(error)
            if "-> HTTP 408" not in message and "-> HTTP 5" not in message:
                raise
            last_error = error
            time.sleep(2.0 * (attempt + 1))
    raise EvalFailure(f"chat/new failed after {attempts} attempts: {last_error}")


def send_message(
    client: Client,
    session_id: str,
    text: str,
    model_selection: dict[str, str] | None = None,
    retries: int = 2,
    *,
    stall_seconds: float = DEFAULT_STALL_SECONDS,
    turn_max_seconds: float = DEFAULT_TURN_MAX_SECONDS,
    progress: Callable[[], Any] | None = None,
    poll_seconds: float = TURN_POLL_SECONDS,
    hitl_probe: Callable[[str], dict[str, Any] | None] | None = None,
) -> dict[str, Any]:
    """Send one customer message and return the turn's assistant message.

    The server holds the POST open for the whole turn, so a lost response is
    not a lost turn: the message is never re-POSTed once it may have been
    accepted (that duplicated the customer's message), and the driver watches
    the turn until it answers or stops making progress. Retries are kept only
    where the server provably did not take the message (connect/send failure,
    HTTP 401/408/5xx).

    With ``hitl_probe`` (the HITL relay), a turn that pauses on a question for
    the customer returns ``{"hitl_question": request, "turn": {...}}`` instead
    of an answer; ``await_turn_by_id(..., **turn)`` resumes the wait once the
    question is answered. The POST then runs on its own thread: a paused turn
    holds it open until the driver answers, so it cannot block the watch."""
    chat_turn_id = f"bench-{secrets.token_hex(8)}"
    body: dict[str, Any] = {"text": text[:MAX_MESSAGE_CHARS], "source_surface": "web", "chat_turn_id": chat_turn_id}
    body.update(chat_model_request_fields(model_selection))
    started = time.monotonic()

    def watch(post_outcome: dict[str, Any] | None = None) -> dict[str, Any]:
        return await_turn_by_id(
            client, session_id, chat_turn_id,
            stall_seconds=stall_seconds, turn_max_seconds=turn_max_seconds,
            progress=progress, poll_seconds=poll_seconds, started=started,
            hitl_probe=hitl_probe, post_outcome=post_outcome,
        )

    def post_turn() -> dict[str, Any] | None:
        """The answer when the POST carried one; None hands off to watching."""
        last_error: Exception | None = None
        for attempt in range(retries + 1):
            try:
                response = client.json(
                    "POST", f"/api/magician/v2/chat/sessions/{session_id}/messages", body,
                    timeout=stall_seconds if stall_seconds > 0 else DEFAULT_STALL_SECONDS,
                )
            except ResponseLost:
                # Sent, response not (yet) received: the turn may be running.
                return None
            except RuntimeUnavailable as error:
                last_error = error
                if attempt < retries:
                    time.sleep(3.0 * (attempt + 1))
                continue
            except EvalFailure as error:
                message = str(error)
                transient = "-> HTTP 401" in message or "-> HTTP 408" in message or "-> HTTP 5" in message
                if transient and attempt < retries:
                    last_error = error
                    time.sleep(2.0)
                    continue
                raise
            if isinstance(response, dict) and response.get("assistant_message"):
                return response
            if isinstance(response, dict) and response.get("cancelled"):
                raise EvalFailure(f"turn {chat_turn_id} was cancelled server-side")
            # Queued, or accepted without an answer in the body: the server has
            # the message either way, so watch rather than send it again.
            return None
        raise RuntimeUnavailable(f"send_message exhausted retries: {last_error}")

    if hitl_probe is None:
        response = post_turn()
        return response if response is not None else watch()
    post_outcome: dict[str, Any] = {"done": False}

    def run_post() -> None:
        try:
            post_outcome["response"] = post_turn()
        except BaseException as error:  # noqa: BLE001 - re-raised on the watching thread
            post_outcome["error"] = error
        finally:
            post_outcome["done"] = True

    threading.Thread(target=run_post, daemon=True, name="tau2-chat-post").start()
    return watch(post_outcome)


def cancel_chat_run(client: Client, session_id: str) -> None:
    """Best effort: stop an abandoned turn so it cannot keep acting (and, here,
    mutating the benchmark environment) after the driver has moved on."""
    try:
        client.request("DELETE", f"/api/magician/v2/chat/sessions/{session_id}/run", timeout=15.0)
    except EvalFailure as error:
        print(f"  warning: cancel of the abandoned turn failed: {error}")


def turn_assistant_message(client: Client, session_id: str, chat_turn_id: str) -> dict[str, Any] | None:
    history = client.json("GET", f"/api/magician/v2/chat/sessions/{session_id}/messages", query={"limit": 50})
    for message in reversed((history or {}).get("messages") or []):
        if message.get("chat_turn_id") == chat_turn_id and message.get("direction") == "assistant":
            return message
    return None


def turn_events_mark(client: Client, session_id: str, chat_turn_id: str) -> tuple[Any, ...]:
    """A value that changes whenever the turn appends an event: the newest row
    plus the scanned row count (the count alone plateaus once the turn file
    outgrows the endpoint's 1 MiB tail window). Also returns the turn's own
    reported cost so an answer recovered by watching keeps its usage."""
    payload = client.json(
        "GET", f"/api/magician/v2/chat/sessions/{session_id}/turns/{chat_turn_id}/events", query={"limit": 1}
    ) or {}
    events = payload.get("events") or []
    newest = json.dumps(events[-1], sort_keys=True) if events else ""
    cost = (payload.get("usage") or {}).get("cost_usd")
    return payload.get("total"), hashlib.sha256(newest.encode()).hexdigest(), cost


def await_turn_by_id(
    client: Client,
    session_id: str,
    chat_turn_id: str,
    *,
    stall_seconds: float = DEFAULT_STALL_SECONDS,
    turn_max_seconds: float = DEFAULT_TURN_MAX_SECONDS,
    progress: Callable[[], Any] | None = None,
    poll_seconds: float = TURN_POLL_SECONDS,
    started: float | None = None,
    hitl_probe: Callable[[str], dict[str, Any] | None] | None = None,
    post_outcome: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Wait for the turn's assistant message, judged by progress not length.

    Progress is a change in the turn's event log or in the tool bridge's
    execution count; either resets the stall clock. Gives up — cancelling the
    turn server-side first — only after `stall_seconds` with no progress, or
    when the turn outlives `turn_max_seconds` (0 = no backstop).

    `post_outcome` is the background POST's result box (relay mode): its
    answer wins and its error is raised here. `hitl_probe` returns a pending
    question for the customer; the wait then stops with ``hitl_question`` and
    the ``turn`` keyword arguments that resume it."""
    started = time.monotonic() if started is None else started
    last_mark: tuple[Any, ...] | None = None
    last_progress_at = time.monotonic()
    cost: Any = None
    while True:
        if post_outcome is not None and post_outcome.get("done"):
            if post_outcome.get("error") is not None:
                raise post_outcome["error"]
            if post_outcome.get("response") is not None:
                return post_outcome["response"]
        try:
            message = turn_assistant_message(client, session_id, chat_turn_id)
            if message is not None:
                usage = {"costUsd": cost} if isinstance(cost, (int, float)) else {}
                return {"assistant_message": message, "usage": usage}
            question = hitl_probe(chat_turn_id) if hitl_probe is not None else None
            # Re-read the history first: a turn that finished while its
            # question was still listed has nothing left to ask.
            if question is not None and turn_assistant_message(client, session_id, chat_turn_id) is None:
                return {
                    "hitl_question": question,
                    "turn": {"chat_turn_id": chat_turn_id, "started": started, "post_outcome": post_outcome},
                }
            total, newest, cost = turn_events_mark(client, session_id, chat_turn_id)
            events_mark: Any = (total, newest)
        except RuntimeUnavailable:
            # A runtime blip is not progress; the stall window decides.
            events_mark = last_mark[0] if last_mark else None
        mark = (events_mark, progress() if progress else None)
        now = time.monotonic()
        if mark != last_mark:
            last_mark, last_progress_at = mark, now
        if turn_max_seconds > 0 and now - started > turn_max_seconds:
            cancel_chat_run(client, session_id)
            raise TurnBackstopExceeded(
                f"turn {chat_turn_id} exceeded the {turn_max_seconds:.0f}s backstop while still progressing"
            )
        if stall_seconds > 0 and now - last_progress_at > stall_seconds:
            cancel_chat_run(client, session_id)
            raise TurnStalled(f"turn {chat_turn_id} stalled: no progress for {stall_seconds:.0f}s")
        time.sleep(poll_seconds)


def assistant_text(response: dict[str, Any]) -> str:
    message = response.get("assistant_message") or {}
    content = message.get("content") or {}
    text = content.get("text")
    if not text:
        presentation = message.get("presentation") or {}
        text = presentation.get("plain_text") or ""
    return str(text)


def usage_cost(response: dict[str, Any]) -> float:
    cost = (response.get("usage") or {}).get("costUsd")
    return float(cost) if isinstance(cost, (int, float)) else 0.0


# ---------------------------------------------------------------------------
# Wire protocol
# ---------------------------------------------------------------------------


def parse_tool_calls(text: str) -> list[dict[str, Any]] | None:
    """Return tool calls if the reply is (or contains) a tool-call JSON
    object; None when the reply is plain speech to the customer."""
    stripped = text.strip()
    candidates: list[str] = []
    if stripped.startswith("{"):
        candidates.append(stripped)
    fence = re.search(r"```(?:json)?\s*(\{.*?\})\s*```", stripped, re.DOTALL)
    if fence:
        candidates.append(fence.group(1))
    for candidate in candidates:
        try:
            payload = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(payload, dict) and "tool_calls" in payload:
            calls = payload["tool_calls"]
            if isinstance(calls, list):
                return [c for c in calls if isinstance(c, dict) and isinstance(c.get("name"), str)]
    return None


def tool_json_schema(tool: Any) -> dict[str, Any]:
    for getter in ("get_json_schema",):
        fn = getattr(tool, getter, None)
        if callable(fn):
            try:
                schema = fn()
                if isinstance(schema, dict):
                    return schema
            except Exception:  # noqa: BLE001 - fall through to the manual build
                pass
    params = getattr(tool, "params", None)
    parameters = params.model_json_schema() if params is not None and hasattr(params, "model_json_schema") else {}
    schema: dict[str, Any] = {"name": getattr(tool, "name", str(tool)), "parameters": parameters}
    description = getattr(tool, "long_desc", "") or getattr(tool, "short_desc", "")
    if description:
        schema["description"] = description
    return schema


# ---------------------------------------------------------------------------
# HITL: relay of the agent's questions + answerer for everything else
# ---------------------------------------------------------------------------

# Request types the chat input tool raises (`need_user_input`; `user_input`
# is the service's generic discriminator). Only these are questions for the
# customer — tool authorizations and other operator prompts are not.
RELAYABLE_REQUEST_TYPES = ("need_user_input", "user_input")


def hitl_request_kind(request: dict[str, Any]) -> str:
    """``confirmation`` | ``choice`` (with options) | ``text`` — every other
    shape (text, form, password …) is answered with free text."""
    context = request.get("context") or {}
    schema = context.get("input_schema") or request.get("input_schema") or {}
    options = request.get("options") or schema.get("options") or []
    input_type = schema.get("input_type") or context.get("input_type") or "choice"
    if input_type == "confirmation":
        return "confirmation"
    if input_type == "choice" and options:
        return "choice"
    return "text"


def is_relayable_question(request: dict[str, Any]) -> bool:
    """A free-text question the agent put to the customer via its input tool."""
    return request.get("request_type") in RELAYABLE_REQUEST_TYPES and hitl_request_kind(request) == "text"


def relay_question_text(request: dict[str, Any]) -> str:
    """The question as the customer should read it; a form's sub-questions
    are listed under the headline."""
    question = str(request.get("question") or "").strip()
    schema = (request.get("context") or {}).get("input_schema") or {}
    extra: list[str] = []
    for item in schema.get("questions") or []:
        if isinstance(item, dict):
            prompt = str(item.get("question") or item.get("label") or item.get("prompt") or "").strip()
            if prompt and prompt != question:
                extra.append(prompt)
    lines = ([question] if question else []) + [f"- {prompt}" for prompt in extra]
    return "\n".join(lines) or "Could you give me a few more details?"


def pending_relay_question(client: Client, session_id: str, skip: set[str] | frozenset[str] = frozenset()) -> dict[str, Any] | None:
    """The oldest relayable question pending in the client's (task) workspace
    for this chat session. Requests carry ``context.chat_session_id``; one
    without it is accepted — the workspace is the task's alone."""
    try:
        payload = client.json("GET", "/api/magician/v2/user-requests")
    except RuntimeUnavailable:
        raise
    except EvalFailure as error:
        print(f"  warning: listing user requests failed: {error}")
        return None
    candidates = []
    for request in (payload or {}).get("requests") or []:
        if not isinstance(request, dict) or not is_relayable_question(request) or request.get("id") in skip:
            continue
        owner_session = (request.get("context") or {}).get("chat_session_id")
        if owner_session and owner_session != session_id:
            continue
        candidates.append(request)
    return min(candidates, key=lambda r: r.get("created_at") or 0) if candidates else None


def respond_hitl_text(client: Client, request_id: str, text: str) -> bool:
    """Post free text as the answer. False when the request is no longer
    pending (answered elsewhere, withdrawn or expired: 404/409/410)."""
    try:
        client.json(
            "POST", f"/api/magician/v2/hitl/{request_id}/respond",
            {"source": "user_request", "value": {"type": "text", "value": text}, "channel": "web"},
            expected=(200, 201, 202, 204),
        )
        return True
    except RuntimeUnavailable:
        raise
    except EvalFailure as error:
        if any(f"-> HTTP {code}" in str(error) for code in (404, 409, 410)):
            return False
        raise


class HitlAnswerer:
    """Answers ``need_user_input`` requests on an independent login session —
    chat turns block on them for up to 24h and a benchmark has no human (the
    first smoke run stalled 20 minutes on "can you confirm the user ID?").
    Confirmations are answered YES so the agent proceeds with its own plan;
    memory-save questions YES; everything else gets minimal-action.

    With ``relay_free_text`` the agent's free-text questions in the active
    workspace are left alone: the turn's own driver relays them to the
    simulated customer and posts the customer's reply as the answer."""

    POLL_SECONDS = 2.0
    CANNED_TEXT = (
        "The customer cannot see this prompt: they only see your chat "
        "replies. Ask the customer any question directly in your reply."
    )
    _SAVE_WORDS = ("save", "yes", "remember", "keep", "store", "summar", "memor", "confirm", "allow", "record")
    _MINIMAL_ACTION_WORDS = ("skip", "none", "continue", "ignore", "not needed", "no action", "proceed", "dismiss")
    _MEMORY_QUESTION_WORDS = ("memory", "save", "summary", "remember", "note")

    def __init__(self, client_factory, relay_free_text: bool = False):
        self._threading = threading
        self._client_factory = client_factory
        self.relay_free_text = relay_free_text
        self._client: Client | None = None
        self._scoped_workspace = ""
        self._workspace = ""
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._failed_attempts: dict[str, int] = {}
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        self._thread = self._threading.Thread(target=self._run, daemon=True, name="tau2-hitl-answerer")
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=5.0)

    def set_workspace(self, slug: str) -> None:
        with self._lock:
            self._workspace = slug

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                self._tick()
            except Exception:  # noqa: BLE001 - the answerer must survive anything
                self._client = None
                self._scoped_workspace = ""
            self._stop.wait(self.POLL_SECONDS)

    def _tick(self) -> None:
        with self._lock:
            workspace = self._workspace
        if not workspace:
            return
        if self._client is None:
            self._client = self._client_factory()
        if self._scoped_workspace != workspace:
            scope_client(self._client, workspace)
            self._scoped_workspace = workspace
        payload = self._client.json("GET", "/api/magician/v2/user-requests")
        for request in (payload or {}).get("requests") or []:
            if self.relay_free_text and is_relayable_question(request) and request.get("workspace", workspace) == workspace:
                continue  # the relay owns it
            self._answer(request)

    def _answer(self, request: dict[str, Any]) -> None:
        request_id = request.get("id") or ""
        if self._failed_attempts.get(request_id, 0) >= 3:
            return
        question = str(request.get("question") or "")[:200]
        schema = (request.get("context") or {}).get("input_schema") or request.get("input_schema") or {}
        options = request.get("options") or schema.get("options") or []
        kind = hitl_request_kind(request)
        if kind == "confirmation":
            # AgenticResumeValue::Confirmation is {"type","confirmed"}.
            value: dict[str, Any] = {"type": "confirmation", "confirmed": True}
            answer_summary = "confirmed"
        elif kind == "choice":
            question_lower = question.lower()
            wants_save = any(word in question_lower for word in self._MEMORY_QUESTION_WORDS)
            words = self._SAVE_WORDS if wants_save else self._MINIMAL_ACTION_WORDS
            preferred = next(
                (o for o in options if any(w in f"{o.get('id', '')} {o.get('label', '')}".lower() for w in words)),
                None,
            )
            chosen = preferred or options[0]
            value = {"type": "choice", "selected_id": chosen.get("id")}
            answer_summary = f"choice:{chosen.get('id')}"
        else:
            value = {"type": "text", "value": self.CANNED_TEXT}
            answer_summary = "text"
        try:
            self._client.json(  # type: ignore[union-attr]
                "POST",
                f"/api/magician/v2/hitl/{request_id}/respond",
                {"source": "user_request", "value": value, "channel": "web"},
                expected=(200, 201, 202, 204),
            )
            print(f"  [hitl] answered ({answer_summary}): {question[:100]}")
        except EvalFailure as error:
            self._failed_attempts[request_id] = self._failed_attempts.get(request_id, 0) + 1
            print(f"  [hitl] FAILED to answer request {request_id}: {error}")


# ---------------------------------------------------------------------------
# Native tool mounting: bridge + memoizing environment wrapper + skills
# ---------------------------------------------------------------------------


class ToolBridge:
    """A localhost HTTP endpoint that turns Magician skills into tau2 tool
    executions. Each mounted skill curl-POSTs its JSON arguments here; the
    bridge executes them for REAL against the live tau2 environment (once)
    and returns the result to the model mid-turn. Every execution is
    memoized per Magician turn so the tau2 orchestrator's replay of the
    reflected tool calls can be answered without re-mutating the
    environment (tau2's evaluator reads actions from the message history,
    so the adapter reflects what the bridge executed)."""

    def __init__(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        self.env: Any = None  # the live tau2 environment for the current task
        self._turn_memo: list[dict[str, Any]] = []  # executions of the current Magician turn
        self._turn_open = False
        self.executions: list[dict[str, Any]] = []  # every execution this run

        bridge = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args: Any) -> None:  # noqa: A003 - silence
                pass

            def do_POST(self) -> None:  # noqa: N802 - http.server API
                import urllib.parse

                name = urllib.parse.urlparse(self.path).path.rsplit("/", 1)[-1]
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b"{}"
                try:
                    arguments = json.loads(body or b"{}")
                except json.JSONDecodeError:
                    arguments = {}
                status, payload = bridge.execute(name, arguments)
                blob = json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(blob)))
                self.end_headers()
                self.wfile.write(blob)

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self._server.server_address[1]
        import threading

        threading.Thread(target=self._server.serve_forever, daemon=True, name="tau2-tool-bridge").start()

    def execute(self, name: str, arguments: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        if self.env is None:
            return 503, {"error": True, "content": "benchmark bridge: no live environment"}
        from tau2.data_model.message import ToolCall

        tool_call = ToolCall(id=f"bridge-{len(self.executions)}", name=name, arguments=arguments)
        result = self.env.get_response(tool_call)
        record = {
            "name": name,
            "arguments": arguments,
            "args_key": json.dumps(arguments, sort_keys=True),
            "content": result.content,
            "error": bool(result.error),
            "at_ms": int(time.time() * 1000),
        }
        self.executions.append(record)
        if self._turn_open:
            self._turn_memo.append(record)
        return 200, {"error": record["error"], "content": record["content"]}

    def begin_turn(self) -> None:
        self._turn_memo = []
        self._turn_open = True

    def end_turn(self) -> list[dict[str, Any]]:
        self._turn_open = False
        memo, self._turn_memo = self._turn_memo, []
        return memo

    def take_replay(self, name: str, arguments: dict[str, Any]) -> dict[str, Any] | None:
        """Called by the wrapped environment when the orchestrator re-executes
        a reflected call: return the memoized result, do not re-mutate."""
        args_key = json.dumps(arguments, sort_keys=True)
        for index, record in enumerate(self._turn_memo):
            if record["name"] == name and record["args_key"] == args_key:
                return self._turn_memo.pop(index)
        return None

    def arm_replay(self, memo: list[dict[str, Any]]) -> None:
        """Give the wrapper the turn's executions to answer orchestrator
        replays from (consumed in order, matched by name+args)."""
        self._turn_memo = list(memo)
        self._turn_open = False

    def stop(self) -> None:
        self._server.shutdown()


class WrappedEnvironment:
    """Delegates everything to the real tau2 environment except
    ``get_response``: reflected calls are answered from the bridge's replay
    pool so the orchestrator's re-execution never mutates twice. Anything
    without a memo match (user tools, initialization actions) executes for
    real."""

    def __init__(self, real: Any, bridge: ToolBridge):
        object.__setattr__(self, "_real", real)
        object.__setattr__(self, "_bridge", bridge)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)

    def get_response(self, tool_call: Any) -> Any:
        from tau2.data_model.message import ToolMessage

        memo = self._bridge.take_replay(tool_call.name, tool_call.arguments)
        if memo is not None:
            return ToolMessage(
                id=tool_call.id,
                role="tool",
                content=memo["content"],
                error=memo["error"],
                requestor=getattr(tool_call, "requestor", "assistant") or "assistant",
            )
        return self._real.get_response(tool_call)


def register_wrapped_domain(registry_module: Any, domain: str, bridge: ToolBridge) -> Any:
    """Swap the domain's environment factory so build_orchestrator hands our
    wrapper to the orchestrator; the real environment lands in the bridge for
    skill execution. register_domain() refuses duplicates, so the swap goes
    through the registry's domain map directly."""
    real_factory = registry_module._domains.get(domain)
    if real_factory is None:
        raise EvalFailure(f"tau2 registry has no environment factory for {domain!r}")

    def factory(*args: Any, **kwargs: Any) -> WrappedEnvironment:
        real = real_factory(*args, **kwargs)
        bridge.env = real
        return WrappedEnvironment(real, bridge)

    registry_module._domains[domain] = factory
    return real_factory


def benchmark_skill_name(tool_name: str) -> str:
    """Benchmark skills are prefixed so they can never collide with (or be
    mistaken for) the assistant's own built-in tools — mock's create_task
    IS a real personal-assistant tool grant, and a duplicate name fails
    agent-definition validation outright."""
    return f"tau2_{tool_name}"


# A string array's declared bounds must fit the governed runtime's combined
# argument budget: max_items x max_item_bytes <= 64 KiB (tool-runtime-core
# MAX_FIXED_ARGUMENT_BYTES). Before tool-runtime-core 0.1.82 an array declared
# without bounds defaulted to 128 x 4 KiB = 512 KiB, the action failed to
# compile, and every list-taking tool (exchange, return, modify items) was
# dropped from the scope at load — tool_search then answered `matches: []`.
STRING_ARRAY_MAX_ITEMS = 64
STRING_ARRAY_MAX_ITEM_BYTES = 256
STRING_ARRAY_COMBINED_BYTES_CEILING = 64 * 1024


def typed_parameter_type(prop: dict[str, Any]) -> str:
    """Map one pydantic JSON-schema property onto the runtime's finite typed
    parameter vocabulary. Optional[...] fields arrive as anyOf [X, null]."""
    ptype = prop.get("type")
    if ptype is None:
        for option in prop.get("anyOf") or prop.get("oneOf") or []:
            if option.get("type") not in (None, "null"):
                prop, ptype = option, option["type"]
                break
    if ptype in ("string", "integer", "number", "boolean"):
        return ptype
    if ptype == "array":
        return "string_array" if (prop.get("items") or {}).get("type") == "string" else "json_array"
    return "json_object"


def render_skill_markdown(tool: Any, port: int) -> str:
    """One single-action governed-CLI skill per tau2 tool: the model's JSON
    arguments are serialized to canonical JSON on curl's stdin and POSTed
    verbatim to the bridge (input_delivery: canonical_json_stdin)."""
    skill_name = benchmark_skill_name(tool.name)
    schema = (tool.params.model_json_schema() if tool.params is not None and hasattr(tool.params, "model_json_schema") else {}) or {}
    properties = schema.get("properties") or {}
    required = set(schema.get("required") or [])
    # Entries sit under `actions.run.parameters` (12-space keys). The runtime's
    # typed-action parser is deny_unknown_fields, so a mis-indented entry lands
    # beside `actions:` and the whole skill is dropped at load — invisibly to
    # the model.
    lines: list[str] = []
    for pname, prop in properties.items():
        mapped = typed_parameter_type(prop)
        entry = f'            {pname}:\n              type: {mapped}\n              description: {json.dumps(str(prop.get("description", pname))[:400])}\n'
        if pname in required:
            entry += "              required: true\n"
        if mapped == "string_array":
            entry += f"              max_items: {STRING_ARRAY_MAX_ITEMS}\n              max_item_bytes: {STRING_ARRAY_MAX_ITEM_BYTES}\n"
        lines.append(entry.rstrip("\n"))
    # A no-argument tool gets an empty map: the bridge forwards the posted
    # object verbatim as tau2 kwargs, so a wrapper parameter would break it.
    parameters = ("          parameters:\n" + "\n".join(lines)) if lines else "          parameters: {}"
    name = tool.name
    description = json.dumps((tool.short_desc or f"tau2 {name} tool")[:300])
    long_desc = (tool.long_desc or tool.short_desc or name).strip()
    return f'''---
name: "{skill_name}"
version: 0.1.0
description: {description}
metadata:
  magician:
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: "benchmark-local endpoint on 127.0.0.1"
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [curl]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {{mode: required, sensitivity: private}}
        working_directory: {{mode: denied}}
        limits: {{timeout_secs: 60, stdin_bytes: 1048576, stdout_bytes: 2097152, stderr_bytes: 262144}}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: "POST the arguments to the benchmark {name} endpoint and return its JSON result."
          fixed_args: ["-sSf", "-X", "POST",
                       "http://127.0.0.1:{port}/tool/{name}",
                       "-H", "Content-Type: application/json",
                       "--data-binary", "@-"]
{parameters}
    runtime_catalog:
      categories: [benchmark]
---

# {name}

{long_desc}

Call this tool with the exact arguments its schema requires; the JSON result
is the authoritative environment response.
'''


def generate_skill_tree(tools: list[Any], port: int, root: Path) -> list[str]:
    """Materialize one skill directory per tool under a staging root."""
    names: list[str] = []
    for tool in tools:
        skill_dir = root / benchmark_skill_name(tool.name)
        skill_dir.mkdir(parents=True, exist_ok=True)
        (skill_dir / "SKILL.md").write_text(render_skill_markdown(tool, port))
        names.append(benchmark_skill_name(tool.name))
    return names


def install_skills(scope_skills_dir: Path, source_root: Path, names: list[str]) -> None:
    """Lay the generated skills directly into the scope's skills dir. The
    install_skill_layer.py installer is the right tool for shared scopes,
    but its --layer vocabulary ("all") and skill_layer.py's ("both") have
    drifted apart, and a fresh benchmark workspace has nothing to swap
    against anyway — the runtime loads <scope>/skills/<skill>/SKILL.md
    straight from disk."""
    import shutil

    scope_skills_dir.mkdir(parents=True, exist_ok=True)
    for name in names:
        source = source_root / name
        if not (source / "SKILL.md").is_file():
            raise EvalFailure(f"generated skill missing: {source}")
        target = scope_skills_dir / name
        if target.exists():
            shutil.rmtree(target)
        shutil.copytree(source, target)


def verify_scope_skills_load(client: Client, slug: str, names: list[str]) -> None:
    """Fail the task before its first turn when the runtime refuses a mounted
    skill. A skill whose frontmatter parses can still fail to compile at load
    (typed action bounds); it then vanishes from the agent's tools and the run
    scores the missing tool as a wrong answer. The catalog lists such skills
    as `invalid` with the loader's reason (servers before this check list
    them as healthy, so this passes vacuously there)."""
    payload = client.json("GET", "/api/magician/v2/skills/catalog")
    label = f"{client.principal}/{slug}"
    wanted = set(names)
    broken = {
        row["name"]: row.get("invalid")
        for row in (payload or {}).get("skills") or []
        if row.get("name") in wanted and row.get("invalid") and label in (row.get("installed_scopes") or [])
    }
    if broken:
        detail = "; ".join(f"{name}: {reason}" for name, reason in sorted(broken.items()))
        raise EvalFailure(f"runtime refused {len(broken)} mounted skill(s) in {label}: {detail}")


AGENT_TEMPLATE = REPO_ROOT /"magician_data_v3/system/agent_templates/agents/personal-assistant/definition.agent.yaml"


def seed_scope_agent_definition(client: Client, slug: str, tool_names: list[str]) -> None:
    """Pre-seed the scope's materialized personal-assistant definition with
    the benchmark tools granted, BEFORE the store's materialize-if-not-exists
    runs (it never overwrites an existing file). The system template is never
    touched, so real scopes are unaffected."""
    if not client.principal or not AGENT_TEMPLATE.is_file():
        raise EvalFailure("cannot seed scope agent definition (no principal or missing template)")
    definition_dir = runtime_root() / "scopes" / client.principal / slug / "agent_runtime/agents/personal-assistant"
    definition_dir.mkdir(parents=True, exist_ok=True)
    target = definition_dir / "definition.agent.yaml"
    if target.exists():
        return
    lines = AGENT_TEMPLATE.read_text().splitlines(keepends=True)
    existing = {line[2:].strip() for line in lines if line.startswith("- ")}
    grant = "".join(f"- {name}\n" for name in tool_names if name not in existing)
    if not grant:
        return
    for index, line in enumerate(lines):
        if line.startswith("tools:"):
            insert = "# --- tau2 benchmark tools (driver-generated) ---\n" + grant
            lines[index + 1:index + 1] = [insert]
            break
    else:
        lines.append("\ntools:\n" + grant)
    target.write_text("".join(lines))


# ---------------------------------------------------------------------------
# The Magician agent
# ---------------------------------------------------------------------------


class DriverContext:
    """Everything the per-task agents share: args, cost and workspace
    registries, the tool bridge, and the generated skill tree."""

    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.cost_by_workspace: dict[str, float] = {}
        self.profiles_seen: set[str] = set()
        self.agents: list[MagicianTauAgent] = []
        self.hitl: HitlAnswerer | None = None
        self.bridge: ToolBridge | None = None
        self.skills_source_root: Path | None = None
        self.skill_names: list[str] = []


try:
    from tau2.agent.base_agent import HalfDuplexAgent as _HalfDuplexAgent
except ImportError:  # running outside tau2's venv (e.g. --help on system python)
    _HalfDuplexAgent = object


class MagicianTauAgent(_HalfDuplexAgent):
    """tau2 HalfDuplexAgent backed by a fresh Magician chat session with the
    domain's tools MOUNTED as native skills.

    Registered as the ``magician_agent`` factory; built once per task run.
    Magician executes tools for real through the bridge mid-turn; the
    adapter reflects those executions as tool-call messages so tau2's
    message-history-based evaluator sees them, and the wrapped environment
    answers the orchestrator's replay from the memo (no double mutation)."""

    def __init__(self, tools: list[Any], domain_policy: str, context: DriverContext, task_id: str):
        if _HalfDuplexAgent is not object:
            super().__init__(tools, domain_policy)
        self._init_state(context, task_id, f"{WORKSPACE_SLUG_PREFIX}-{re.sub(r'[^a-z0-9_-]', '-', task_id.lower())[:16]}-{secrets.token_hex(2)}")
        self._client = login_client(context.args)
        session = self._client.json("GET", "/api/magician/v2/auth/session")
        self._client.principal = str(session.get("principal") or "")
        create_workspace(self._client, self.slug)
        # The sidecar must watch this workspace BEFORE the opening send —
        # the opening turn itself can raise a user request (seen live:
        # "please provide the email to authenticate") and nothing else will
        # answer it for 24 hours.
        if context.hitl:
            context.hitl.set_workspace(self.slug)
        # Mount the domain's tools natively BEFORE the first chat session:
        # skills into the scope (live pickup), the tool grant into the
        # scope-materialized agent definition (written only if absent).
        if context.skills_source_root and context.skill_names:
            scope_dir = runtime_root() / "scopes" / self._client.principal / self.slug
            install_skills(scope_dir / "skills", context.skills_source_root, context.skill_names)
            seed_scope_agent_definition(self._client, self.slug, context.skill_names)
            verify_scope_skills_load(self._client, self.slug, context.skill_names)
        scope_client(self._client, self.slug)
        self.session_id = new_chat_session(self._client, f"tau2-{self.slug}")
        opening = self._opening_message(tools, domain_policy)
        # With the relay on, a question the opening turn asks stays pending:
        # the customer's first message (tau2 speaks next) answers it.
        self._send(opening)

    # -- tau2 contract ------------------------------------------------------

    def get_init_state(self, message_history: list[Any] | None = None) -> None:  # noqa: ARG002
        return None

    def generate_next_message(self, message: Any, state: Any) -> tuple[Any, None]:
        from tau2.data_model.message import AssistantMessage, MultiToolMessage, ToolCall, UserMessage

        if isinstance(message, (MultiToolMessage,)) or type(message).__name__ == "ToolMessage":
            # Orchestrator replayed the reflected calls; Magician already
            # consumed the real results inside its turn — return the final
            # text it produced then, without another model call.
            text = self._pending_text or ""
            self._pending_text = None
            return AssistantMessage.text(content=text), None

        assert isinstance(message, UserMessage), f"unexpected message type {type(message)}"
        bridge = self.context.bridge
        bridge.begin_turn()
        try:
            # A turn paused on a relayed question takes this message as the
            # answer and carries on; otherwise it opens a new turn. Either way
            # the reply may itself be the next relayed question.
            if self._relay is not None:
                reply = self._answer_relayed(message.content or "")
            else:
                reply = self._send(f"CUSTOMER: {message.content or ''}")
        except BaseException:
            # An abandoned turn still acted: count what the bridge executed
            # so the task record reflects it instead of reporting zero calls.
            self.tool_calls_made += len(bridge.end_turn())
            raise
        executed = bridge.end_turn()
        if executed:
            self.tool_calls_made += len(executed)
            bridge.arm_replay(executed)
            self._pending_text = reply
            tool_calls = [
                ToolCall(id=f"call_{index}", name=record["name"], arguments=record["arguments"])
                for index, record in enumerate(executed)
            ]
            return AssistantMessage.text(content="", tool_calls=tool_calls), None
        return AssistantMessage.text(content=reply), None

    # -- internals ----------------------------------------------------------

    def _init_state(self, context: Any, task_id: str, slug: str) -> None:
        self.context = context
        self.task_id = task_id
        self.slug = slug
        self.session_id = ""
        self.cost = 0.0
        self.turns = 0
        self.tool_calls_made = 0
        self.hitl_questions_relayed = 0
        self.relay_log: list[dict[str, Any]] = []
        self._pending_text: str | None = None
        # The relayed question the paused turn waits on: request_id, the
        # question text and the `turn` kwargs that resume the wait.
        self._relay: dict[str, Any] | None = None
        self._answered_request_ids: set[str] = set()
        bridge = getattr(context, "bridge", None)
        self._bridge_start = len(bridge.executions) if bridge is not None else 0

    def _relay_enabled(self) -> bool:
        return bool(getattr(self.context.args, "hitl_relay", True))

    def _wait_kwargs(self) -> dict[str, Any]:
        args = self.context.args
        bridge = getattr(self.context, "bridge", None)
        return {
            "stall_seconds": float(getattr(args, "stall_seconds", DEFAULT_STALL_SECONDS)),
            "turn_max_seconds": float(getattr(args, "turn_max_seconds", DEFAULT_TURN_MAX_SECONDS)),
            "progress": (lambda: len(bridge.executions)) if bridge is not None else None,
            "poll_seconds": float(getattr(args, "turn_poll_seconds", TURN_POLL_SECONDS)),
        }

    def _hitl_probe(self, chat_turn_id: str) -> dict[str, Any] | None:  # noqa: ARG002 - matched by session
        return pending_relay_question(self._client, self.session_id, self._answered_request_ids)

    def _send(self, text: str) -> str:
        response = send_message(
            self._client, self.session_id, text, model_selection=self.context.args.answer_model_selection,
            hitl_probe=self._hitl_probe if self._relay_enabled() else None,
            **self._wait_kwargs(),
        )
        return self._absorb(response)

    def _await_turn(self, turn: dict[str, Any]) -> dict[str, Any]:
        """Resume waiting on a turn that paused on a relayed question."""
        return await_turn_by_id(
            self._client, self.session_id, turn["chat_turn_id"],
            started=turn["started"], post_outcome=turn["post_outcome"],
            hitl_probe=self._hitl_probe, **self._wait_kwargs(),
        )

    def _absorb(self, response: dict[str, Any]) -> str:
        """The text for the customer: the turn's answer, or the question the
        paused turn is asking them (remembered so the next customer message
        answers it)."""
        question = response.get("hitl_question")
        if question is not None:
            text = relay_question_text(question)
            self._relay = {"request_id": str(question.get("id") or ""), "question": text, "turn": response["turn"]}
            self.hitl_questions_relayed += 1
            self.relay_log.append({"ts": int(time.time() * 1000), "kind": "hitl_relay_question", "request_id": self._relay["request_id"], "text": text})
            print(f"  [hitl] relayed to the customer: {text[:100]}")
            return text
        self._relay = None
        self.cost += usage_cost(response)
        self.turns += 1
        profile = str((response.get("usage") or {}).get("profile") or "")
        if profile:
            self.context.profiles_seen.add(profile)
        return assistant_text(response)

    def _answer_relayed(self, text: str) -> str:
        """Post the customer's words as the answer to the paused turn's
        question and keep waiting on the SAME turn. When the question is
        gone (answered elsewhere, withdrawn, expired, or the turn finished
        meanwhile) let the turn settle and send the words as an ordinary
        message instead — or, if the settling turn asks a fresh question,
        answer that one with them."""
        relay, self._relay = self._relay, None
        for _attempt in range(3):
            turn = relay["turn"]
            finished = turn_assistant_message(self._client, self.session_id, turn["chat_turn_id"])
            if finished is None and respond_hitl_text(self._client, relay["request_id"], text):
                self._answered_request_ids.add(relay["request_id"])
                self.relay_log.append({"ts": int(time.time() * 1000), "kind": "hitl_relay_answer", "request_id": relay["request_id"], "text": text})
                return self._absorb(self._await_turn(turn))
            print(f"  [hitl] relayed question {relay['request_id']} is no longer pending; falling back to a chat message")
            settled = self._await_turn(turn)
            question = settled.get("hitl_question")
            if question is None:
                orphan = self._absorb(settled)
                if orphan:
                    print(f"  warning: the turn's reply never reached the customer: {orphan[:100]}")
                return self._send(f"CUSTOMER: {text}")
            relay = {"request_id": str(question.get("id") or ""), "question": relay_question_text(question), "turn": settled["turn"]}
        raise EvalFailure("the relayed question could not be answered after 3 attempts")

    def transcript_extras(self) -> list[dict[str, Any]]:
        """Driver-side entries for the transcript: the bridge executions this
        task made and the relay's questions/answers, all timestamped."""
        bridge = getattr(self.context, "bridge", None)
        executions = bridge.executions[self._bridge_start:] if bridge is not None else []
        entries = [
            {"ts": record.get("at_ms"), "kind": "tool_bridge", "name": record["name"], "arguments": record["arguments"],
             "error": record["error"], "content": record["content"]}
            for record in executions
        ]
        return entries + list(self.relay_log)

    def _opening_message(self, tools: list[Any], domain_policy: str) -> str:
        names = ", ".join(benchmark_skill_name(tool.name) for tool in tools)
        return (
            "You are now working as a customer service agent. Treat this whole "
            "session as your work console for one customer conversation.\n\n"
            "You ARE the service's only agent: there is no other support "
            "channel, no specialist and no handoff to fall back on. Never "
            "mention transfers, unavailable agents or other channels — act "
            "yourself, with your tools.\n\n"
            f"SERVICE POLICY (follow it exactly):\n{domain_policy}\n\n"
            f"YOUR TOOLS are mounted in this workspace as first-class skills: "
            f"{names}. They are live, real tools connected to the service's "
            "systems — call them directly whenever the policy calls for a "
            "lookup or action; never claim you cannot access them. If a tool "
            "is not yet in your visible tool list, use tool_search to bring "
            "it into your working set first.\n\n"
            "Speak to the customer in plain text. Ask for anything the policy "
            "requires (name, zip, email…) directly in your reply. This is an "
            "automated work console: never create reminders/notes/tasks of "
            "your own and never raise approval prompts. Resolve the "
            "customer's issue fully, then confirm.\n\n"
            "Acknowledge briefly; the customer will start shortly."
        )

    def close(self) -> None:
        self.context.cost_by_workspace[self.slug] = self.cost
        if self._relay is not None and self._client is not None:
            # The conversation ended on a question the customer never
            # answered: cancel the paused turn (which withdraws the ask).
            cancel_chat_run(self._client, self.session_id)
            self._relay = None


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


def git_commit() -> str:
    try:
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, timeout=10).stdout.strip()
    except Exception:  # noqa: BLE001
        return "unknown"


def extract_reward(simulation_run: Any) -> dict[str, Any]:
    """Pull reward fields off the SimulationRun. tau2 sets ``reward_info``
    (RewardInfo: .reward, .reward_breakdown) after evaluation; keep a
    defensive walk so a field move degrades to a null, not a crash."""
    found: dict[str, Any] = {"reward": None, "breakdown": None}
    candidates = [getattr(simulation_run, "reward_info", None)]
    evaluation = getattr(simulation_run, "evaluation", None)
    if evaluation is not None:
        candidates.append(evaluation)
    candidates.append(simulation_run)
    for candidate in candidates:
        if candidate is None:
            continue
        reward = getattr(candidate, "reward", None)
        if isinstance(reward, (int, float)):
            found["reward"] = float(reward)
            breakdown = getattr(candidate, "reward_breakdown", None)
            if breakdown:
                try:
                    found["breakdown"] = {str(k): float(v) for k, v in breakdown.items()}
                except Exception:  # noqa: BLE001
                    found["breakdown"] = str(breakdown)[:300]
            break
    for field in ("agent_cost", "user_cost", "duration"):
        value = getattr(simulation_run, field, None)
        if isinstance(value, (int, float)):
            found[field] = float(value)
    return found


def fetch_session_messages(client: Client, session_id: str) -> list[dict[str, Any]]:
    """Every stored message of the session, paging back with ``before``."""
    seen: dict[str, dict[str, Any]] = {}
    before: str | None = None
    for _page in range(TRANSCRIPT_MAX_MESSAGE_PAGES):
        payload = client.json(
            "GET", f"/api/magician/v2/chat/sessions/{session_id}/messages", query={"limit": 200, "before": before}
        ) or {}
        page = [m for m in payload.get("messages") or [] if isinstance(m, dict)]
        for message in page:
            seen.setdefault(str(message.get("id") or len(seen)), message)
        if not payload.get("has_more") or not page:
            break
        oldest = min(page, key=lambda m: m.get("created_at") or 0)
        if not oldest.get("id") or oldest.get("id") == before:
            break
        before = str(oldest["id"])
    return list(seen.values())


def _epoch_ms(value: Any) -> int | None:
    return int(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else None


def turn_event_timestamp(event: dict[str, Any]) -> int | None:
    """Turn-event rows are ``{"event_type", "data"}``; the time sits on
    ``data.timestamp``, an agent event's ``data.event.timestamp``, or a
    message's ``created_at`` (epoch ms)."""
    data = event.get("data") if isinstance(event.get("data"), dict) else {}
    inner = data.get("event") if isinstance(data.get("event"), dict) else {}
    message = data.get("message") if isinstance(data.get("message"), dict) else {}
    for value in (data.get("timestamp"), inner.get("timestamp"), message.get("created_at"), event.get("timestamp")):
        if _epoch_ms(value) is not None:
            return _epoch_ms(value)
    return None


def export_transcript(client: Client, session_id: str, path: Path, *, meta: dict[str, Any], extra_entries: list[dict[str, Any]]) -> Path:
    """Write the task's chat transcript — every message, every turn event
    (tool calls included) and the driver's own entries — as one timeline
    sorted by timestamp. Read before the workspace purge deletes it."""
    messages = fetch_session_messages(client, session_id)
    turn_ids = sorted({str(m["chat_turn_id"]) for m in messages if m.get("chat_turn_id")})
    entries: list[dict[str, Any]] = []
    for message in messages:
        entries.append({
            "ts": _epoch_ms(message.get("created_at")), "kind": "message", "direction": message.get("direction"),
            "chat_turn_id": message.get("chat_turn_id"), "text": assistant_text({"assistant_message": message}),
            "message": message,
        })
    turns: dict[str, Any] = {}
    for turn_id in turn_ids:
        payload = client.json(
            "GET", f"/api/magician/v2/chat/sessions/{session_id}/turns/{turn_id}/events", query={"limit": TRANSCRIPT_EVENTS_LIMIT}
        ) or {}
        events = [e for e in payload.get("events") or [] if isinstance(e, dict)]
        turns[turn_id] = {"events": len(events), "truncated": bool(payload.get("truncated")), "usage": payload.get("usage")}
        for event in events:
            data = event.get("data") if isinstance(event.get("data"), dict) else {}
            inner = data.get("event") if isinstance(data.get("event"), dict) else {}
            entries.append({
                "ts": turn_event_timestamp(event), "kind": "turn_event", "chat_turn_id": turn_id,
                "event_type": inner.get("event_type") or event.get("event_type"), "event": event,
            })
    entries.extend(extra_entries)
    entries.sort(key=lambda entry: (entry.get("ts") is None, entry.get("ts") or 0))
    path.parent.mkdir(parents=True, exist_ok=True)
    document = {"meta": meta, "session_id": session_id, "turns": turns, "entries": entries}
    path.write_text(json.dumps(document, indent=2, default=str))
    return path


def finish_task_workspace(driver_client: Client, agent: MagicianTauAgent, output_dir: Path, *, keep_workspace: bool) -> dict[str, Any]:
    """Close the task's agent, export its transcript, then purge its
    workspace (unless kept). The export runs FIRST — the purge deletes the
    turn files it reads — and a failed export only warns."""
    agent.close()
    record: dict[str, Any] = {
        "cost_usd": round(agent.cost, 4),
        "turns": agent.turns,
        "tool_calls_made": agent.tool_calls_made,
        "hitl_questions_relayed": agent.hitl_questions_relayed,
        "workspace": agent.slug,
    }
    safe_task = re.sub(r"[^A-Za-z0-9_.-]", "-", str(agent.task_id))[:60]
    transcript_path = output_dir / "transcripts" / f"{safe_task}-{agent.slug}.json"
    try:
        if not agent.session_id or agent._client is None:
            raise EvalFailure("the task has no chat session")
        export_transcript(
            agent._client, agent.session_id, transcript_path,
            meta={"task_id": agent.task_id, "workspace": agent.slug, "exported_at": time.strftime("%Y-%m-%dT%H:%M:%S%z")},
            extra_entries=agent.transcript_extras(),
        )
        record["transcript"] = str(transcript_path)
    except Exception as error:  # noqa: BLE001 - an export failure must never fail the task
        print(f"  warning: transcript export for {agent.slug} failed: {error}")
    if keep_workspace:
        record["workspace_kept"] = True
        return record
    try:
        mark_workspace(driver_client, agent.slug)
        purge_workspace(driver_client, agent.slug)
    except EvalFailure as error:
        print(f"  warning: purge of {agent.slug} failed: {error}")
    return record


def write_report(output_dir: Path, args: argparse.Namespace, environment: dict[str, Any], results: list[dict[str, Any]], status: str, profiles: set[str]) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    judged = [r for r in results if r.get("reward") is not None]
    summary = {
        "n_tasks": len(results),
        "n_errors": sum(1 for r in results if r.get("error")),
        "n_rewarded": len(judged),
        "pass_at_1": round(sum(1 for r in judged if r["reward"] >= 1.0) / len(judged), 4) if judged else None,
        "mean_reward": round(sum(r["reward"] for r in judged) / len(judged), 4) if judged else None,
        "cost_usd": round(sum(r.get("cost_usd") or 0.0 for r in results), 4),
        "agent_turns_total": sum(r.get("turns") or 0 for r in results),
        "hitl_questions_relayed_total": sum(r.get("hitl_questions_relayed") or 0 for r in results),
        "avg_task_wall_seconds": round(sum(r.get("wall_seconds") or 0.0 for r in results) / len(results), 1) if results else None,
    }
    report = {
        "kind": "tau2",
        "benchmark": f"tau2-bench ({args.domain} domain, base split)",
        "status": status,
        "verdict": "pass" if status == "completed" and judged else "fail",
        "environment": environment,
        "protocol": {
            "agent": "magician_agent (domain tools mounted natively as skills via the tool bridge)",
            "hitl": "free-text questions relayed to the user simulator" if getattr(args, "hitl_relay", True) else "free-text questions auto-answered (relay off)",
            "user_llm": args.user_llm or "tau2 default",
            "answer_model_selection": args.answer_model_selection or "server default",
        },
        "profiles_seen": sorted(profiles),
        "summary": summary,
        "tasks": results,
    }
    report_path = output_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2))
    rows = "".join(
        "<tr>"
        f"<td>{escape(str(r['task_id']))}</td><td>{r['reward'] if r.get('reward') is not None else '—'}</td>"
        f"<td>${r.get('cost_usd') or 0:.3f}</td><td>{r.get('turns') or 0}</td><td>{r.get('hitl_questions_relayed') or 0}</td>"
        f"<td>{escape(str(r.get('error') or '')[:80])}</td></tr>"
        for r in results
    )
    html = f"""<!doctype html><html><head><meta charset="utf-8"><title>tau2 — Magician</title>
<style>body{{font-family:ui-sans-serif,system-ui;margin:24px}}table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:4px 8px;font-size:13px}}</style></head><body>
<h2>tau2-bench ({escape(args.domain)}) — Magician agent</h2>
<p><b>{escape(status)}</b> · pass@1 <b>{summary['pass_at_1']}</b> · mean reward {summary['mean_reward']}
· {summary['n_rewarded']}/{summary['n_tasks']} rewarded · cost ${summary['cost_usd']}</p>
<table><tr><th>task</th><th>reward</th><th>cost</th><th>turns</th><th>relayed questions</th><th>error</th></tr>{rows}</table></body></html>"""
    (output_dir / "report.html").write_text(html)
    return report_path


def run(args: argparse.Namespace) -> int:
    try:
        resolve_chat_model_selections(args, ("answer",),
            lambda: bootstrap_client(args).json("GET", "/api/magician/v2/chat/models", query={"engine": "magician"}))
    except ValueError as error:
        raise EvalFailure(str(error)) from error
    exported = export_runtime_provider_keys()
    from tau2 import registry
    from tau2.data_model.simulation import TextRunConfig
    from tau2.run import get_tasks, run_single_task

    context = DriverContext(args)
    context.hitl = HitlAnswerer(lambda: login_client(args), relay_free_text=args.hitl_relay)
    context.hitl.start()

    # Native tool mounting: wrap the domain environment (orchestrator gets
    # the memoizing proxy, bridge gets the real env), then generate one
    # governed-CLI skill per domain tool pointing at the bridge port.
    context.bridge = ToolBridge()
    real_factory = register_wrapped_domain(registry, args.domain, context.bridge)
    scratch_env = real_factory()
    domain_tools = scratch_env.get_tools()
    skills_root = Path(tempfile.mkdtemp(prefix="tau2-skills-"))
    context.skill_names = generate_skill_tree(domain_tools, context.bridge.port, skills_root)
    context.skills_source_root = skills_root
    print(f"bridge on 127.0.0.1:{context.bridge.port}; mounted {len(context.skill_names)} tools as skills")

    def factory(tools: list[Any], domain_policy: str, **_kwargs: Any) -> MagicianTauAgent:
        task = _kwargs.get("task")
        task_id = getattr(task, "id", None) or f"task-{secrets.token_hex(2)}"
        agent = MagicianTauAgent(tools, domain_policy, context, task_id)
        if context.hitl:
            context.hitl.set_workspace(agent.slug)
        context.agents.append(agent)
        return agent

    registry.register_agent_factory(factory, "magician_agent")

    task_ids = [t.strip() for t in args.task_ids.split(",") if t.strip()] or None
    tasks = get_tasks(args.domain, task_ids=task_ids, num_tasks=args.limit or None)
    if not tasks:
        print(f"error: no tasks for domain {args.domain!r}")
        return 2

    driver_client = bootstrap_client(args)
    parking = f"{WORKSPACE_SLUG_PREFIX}-parking-{secrets.token_hex(3)}"
    create_workspace(driver_client, parking)
    scope_client(driver_client, parking)
    environment = {
        "base_url": args.base_url,
        "principal": driver_client.principal,
        "magician_version": magician_version(driver_client),
        "git_commit": git_commit(),
        "domain": args.domain,
        "tasks": len(tasks),
        "exported_provider_keys": exported,
        "tool_mounting": "native skills (governed-CLI -> localhost tool bridge)",
        "hitl_relay": bool(args.hitl_relay),
        "keep_workspaces": bool(args.keep_workspaces),
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    print(f"tau2: {len(tasks)} tasks in {args.domain}, agent=magician_agent, user_llm={args.user_llm or 'default'}")
    print(f"environment: {json.dumps(environment)}")

    config_kwargs: dict[str, Any] = {"domain": args.domain, "agent": "magician_agent"}
    if args.user_llm:
        config_kwargs["llm_user"] = args.user_llm
    config = TextRunConfig(**config_kwargs)
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    results_path = output_dir / "results.jsonl"
    results: list[dict[str, Any]] = []
    status = "completed"
    try:
        for index, task in enumerate(tasks, 1):
            task_id = str(getattr(task, "id", index))
            print(f"[{index}/{len(tasks)}] {task_id} …")
            started = time.perf_counter()
            record: dict[str, Any] = {"task_id": task_id, "seed": args.seed + index}
            try:
                simulation_run = run_single_task(config, task, seed=args.seed + index)
                reward = extract_reward(simulation_run)
                record.update(reward)
            except RuntimeUnavailable as error:
                print(f"  runtime unavailable: {error}")
                record["error"] = f"runtime_unavailable: {error}"
                status = "aborted"
            except Exception as error:  # noqa: BLE001 - a task crash must not kill the run
                record["error"] = f"{type(error).__name__}: {error}"
                print(f"  task failed: {record['error'][:160]}")
            agent = context.agents[-1] if context.agents else None
            if agent is not None:
                record.update(finish_task_workspace(driver_client, agent, output_dir, keep_workspace=args.keep_workspaces))
                context.agents.clear()
            record["wall_seconds"] = round(time.perf_counter() - started, 1)
            results.append(record)
            with results_path.open("a") as handle:
                handle.write(json.dumps(record) + "\n")
            print(f"  reward={record.get('reward')} cost=${record.get('cost_usd') or 0:.3f} ({record.get('turns') or 0} turns, {record.get('hitl_questions_relayed') or 0} relayed questions, {record['wall_seconds']}s)")
            spent = sum(r.get("cost_usd") or 0.0 for r in results)
            if spent > args.max_cost_usd:
                print(f"cost ceiling ${args.max_cost_usd} exceeded (${spent:.2f}) — stopping")
                status = "aborted"
                break
    finally:
        if context.hitl:
            context.hitl.stop()
        if context.bridge:
            context.bridge.stop()
        environment["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        report_path = write_report(output_dir, args, environment, results, status, context.profiles_seen)
        purge_workspace(driver_client, parking)  # engraved scope: last call on this session
        print(f"report: {report_path}")
    return 0 if status == "completed" and any(r.get("reward") is not None for r in results) else 1


def magician_version(client: Client) -> str:
    try:
        status, raw = client.request("GET", "/health", timeout=10.0)
        return str((json.loads(raw) or {}).get("version", "unknown")) if status == 200 else "unknown"
    except Exception:  # noqa: BLE001
        return "unknown"


# ---------------------------------------------------------------------------
# Self-test (provider-free)
# ---------------------------------------------------------------------------


class FakeInboundClient:
    """Stands in for the HTTP client inside MagicianTauAgent for the
    self-test: canned replies, no network, no server."""

    replies: list[str] = []


def self_test(args: argparse.Namespace) -> int:
    import tempfile
    from types import SimpleNamespace

    from tau2.data_model.message import AssistantMessage, MultiToolMessage, ToolMessage, ToolCall, UserMessage

    # --- bridge: real execution once, memoized, replay answered without
    # re-execution ---------------------------------------------------------
    class FakeEnv:
        def __init__(self):
            self.executed: list[str] = []

        def get_response(self, tool_call):
            self.executed.append(tool_call.name)
            return ToolMessage(id=tool_call.id, role="tool", content=f'{{"ok": true, "echo": "{tool_call.name}"}}', error=False)

    env = FakeEnv()
    bridge = ToolBridge()
    bridge.env = env
    status, payload = bridge.execute("find_user_id_by_name_zip", {"first_name": "Yusuf"})
    assert status == 200 and payload["error"] is False
    # begin/end turn capture
    bridge.begin_turn()
    bridge.execute("get_order_details", {"order_id": "W1"})
    memo = bridge.end_turn()
    assert [m["name"] for m in memo] == ["get_order_details"]
    # replay is answered from the memo without touching the env again
    before = list(env.executed)
    bridge.arm_replay(memo)
    wrapped = WrappedEnvironment(env, bridge)
    replay_call = ToolCall(id="call_0", name="get_order_details", arguments={"order_id": "W1"})
    result = wrapped.get_response(replay_call)
    assert result.id == "call_0" and "get_order_details" in (result.content or "") and not result.error
    assert env.executed == before, "replay must not re-execute"
    # unmatched calls execute for real (user tools / initialization)
    wrapped.get_response(ToolCall(id="u1", name="user_tool", arguments={}))
    assert env.executed[-1] == "user_tool"
    bridge.stop()

    # --- turn wait: judged by progress, never re-POSTed -----------------------
    class FakeTurnClient:
        """Scripted server: the POST's outcome, how many history polls pass
        before the assistant message lands, and whether the turn's event log
        keeps growing."""

        def __init__(self, post_error: Exception | None, answer_after_polls: int | None, events_grow: bool):
            self.post_error = post_error
            self.answer_after_polls = answer_after_polls
            self.events_grow = events_grow
            self.posts: list[str] = []
            self.history_polls = 0
            self.event_polls = 0
            self.cancels = 0
            self.turn_id = ""

        def json(self, method: str, path: str, body: Any = None, query: Any = None, expected: Any = (200,), timeout: float = 120.0) -> Any:
            if method == "POST":
                self.posts.append(path)
                self.turn_id = body["chat_turn_id"]
                if self.post_error is not None:
                    raise self.post_error
                return {"queued": True}
            if path.endswith("/messages"):
                self.history_polls += 1
                if self.answer_after_polls is not None and self.history_polls > self.answer_after_polls:
                    return {"messages": [{"chat_turn_id": self.turn_id, "direction": "assistant", "content": {"text": "done"}}]}
                return {"messages": []}
            if path.endswith("/events"):
                self.event_polls += 1
                newest = self.event_polls if self.events_grow else 0
                return {"total": newest, "events": [{"n": newest}], "usage": {"cost_usd": 0.25}}
            raise AssertionError(f"unexpected {method} {path}")

        def request(self, method: str, path: str, body: Any = None, query: Any = None, timeout: float = 120.0) -> tuple[int, bytes]:
            assert method == "DELETE" and path.endswith("/run"), (method, path)
            self.cancels += 1
            return 200, b"{}"

    # A lost POST response hands off to watching: one POST, never two.
    lost = FakeTurnClient(ResponseLost("POST: timed out"), answer_after_polls=3, events_grow=True)
    answer = send_message(lost, "s1", "hi", stall_seconds=5.0, turn_max_seconds=0, poll_seconds=0.0)  # type: ignore[arg-type]
    assert len(lost.posts) == 1, lost.posts
    assert assistant_text(answer) == "done" and usage_cost(answer) == 0.25 and lost.cancels == 0
    # A queued POST is watched the same way.
    queued = FakeTurnClient(None, answer_after_polls=1, events_grow=True)
    assert assistant_text(send_message(queued, "s1", "hi", stall_seconds=5.0, turn_max_seconds=0, poll_seconds=0.0)) == "done"  # type: ignore[arg-type]
    assert len(queued.posts) == 1
    # Silence past the stall window: cancel server-side, raise TurnStalled.
    silent = FakeTurnClient(ResponseLost("POST: timed out"), answer_after_polls=None, events_grow=False)
    try:
        send_message(silent, "s1", "hi", stall_seconds=0.05, turn_max_seconds=0, poll_seconds=0.01)  # type: ignore[arg-type]
        raise AssertionError("a silent turn must stall")
    except TurnStalled as error:
        assert "no progress" in str(error), error
    assert silent.cancels == 1 and len(silent.posts) == 1
    # Steady progress outlasts many stall windows: event growth, or a bridge
    # execution with the event log frozen, each resets the stall clock.
    busy = FakeTurnClient(None, answer_after_polls=40, events_grow=True)
    assert assistant_text(send_message(busy, "s1", "hi", stall_seconds=0.05, turn_max_seconds=0, poll_seconds=0.01)) == "done"  # type: ignore[arg-type]
    assert busy.cancels == 0
    executions = iter(range(10_000))
    tool_busy = FakeTurnClient(None, answer_after_polls=40, events_grow=False)
    assert assistant_text(send_message(tool_busy, "s1", "hi", stall_seconds=0.05, turn_max_seconds=0, poll_seconds=0.01, progress=lambda: next(executions))) == "done"  # type: ignore[arg-type]
    assert tool_busy.cancels == 0
    # The backstop still ends a turn that keeps progressing forever.
    endless = FakeTurnClient(None, answer_after_polls=None, events_grow=True)
    try:
        send_message(endless, "s1", "hi", stall_seconds=5.0, turn_max_seconds=0.1, poll_seconds=0.01)  # type: ignore[arg-type]
        raise AssertionError("the backstop must end an endless turn")
    except TurnBackstopExceeded:
        pass
    assert endless.cancels == 1
    # A connect failure (never accepted) is the one case that is re-sent.
    refused = FakeTurnClient(RuntimeUnavailable("POST: connection refused"), answer_after_polls=None, events_grow=False)
    try:
        send_message(refused, "s1", "hi", retries=1, stall_seconds=5.0, poll_seconds=0.0)  # type: ignore[arg-type]
        raise AssertionError("an unreachable runtime must surface")
    except RuntimeUnavailable as error:
        assert "exhausted retries" in str(error), error
    assert len(refused.posts) == 2

    # --- skill generation ----------------------------------------------------
    class FakeParams:
        @staticmethod
        def model_json_schema():
            return {
                "properties": {
                    "expression": {"type": "string", "description": "the expression"},
                    "factor": {"type": "integer", "description": "a number"},
                },
                "required": ["expression"],
            }

    tool = SimpleNamespace(name="calculate", short_desc="Calculate things", long_desc="Long description.", params=FakeParams)
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        names = generate_skill_tree([tool], 8901, root)
        assert names == ["tau2_calculate"]
        markdown = (root / "tau2_calculate/SKILL.md").read_text()
        assert 'name: "tau2_calculate"' in markdown
        assert "http://127.0.0.1:8901/tool/calculate" in markdown
        assert "canonical_json_stdin" in markdown
        assert "exempt:" in markdown  # canary exemption, not a live canary
        # Parse it the way the runtime does: runtime_actions is
        # deny_unknown_fields, so any stray key drops the whole skill.
        import yaml

        def run_action(text: str) -> dict[str, Any]:
            front = yaml.safe_load(text.split("---", 2)[1])
            actions = front["metadata"]["magician"]["runtime_actions"]
            assert set(actions) == {"schema_version", "input_delivery", "actions"}, sorted(actions)
            assert set(actions["actions"]) == {"run"}
            return actions["actions"]["run"]

        params = run_action(markdown)["parameters"]
        assert params == {
            "expression": {"type": "string", "description": "the expression", "required": True},
            "factor": {"type": "integer", "description": "a number"},
        }, params
        bare = SimpleNamespace(name="get_users", short_desc="Get users", long_desc="", params=None)
        assert run_action(render_skill_markdown(bare, 8901))["parameters"] == {}

        # A list parameter must carry bounds inside the runtime's combined
        # argument budget, or the action fails to compile and the tool
        # vanishes from the scope (retail exchange/return/modify-items).
        class ListParams:
            @staticmethod
            def model_json_schema():
                return {
                    "properties": {"item_ids": {"type": "array", "items": {"type": "string"}, "description": "ids"}},
                    "required": ["item_ids"],
                }

        listed = SimpleNamespace(name="exchange_items", short_desc="Exchange", long_desc="", params=ListParams)
        item_ids = run_action(render_skill_markdown(listed, 8901))["parameters"]["item_ids"]
        assert item_ids["type"] == "string_array", item_ids
        assert item_ids["max_items"] * item_ids["max_item_bytes"] <= STRING_ARRAY_COMBINED_BYTES_CEILING, item_ids
        assert typed_parameter_type({"anyOf": [{"type": "string"}, {"type": "null"}]}) == "string"
        assert typed_parameter_type({"type": "array", "items": {"type": "string"}}) == "string_array"
        assert typed_parameter_type({"type": "array", "items": {"type": "object"}}) == "json_array"
        # install_skills lays skills straight into the scope dir
        mod_scope = root / "scope-skills"
        install_skills(mod_scope, root, names)
        assert (mod_scope / "tau2_calculate/SKILL.md").is_file()

    # --- scope agent definition seeding --------------------------------------
    with tempfile.TemporaryDirectory() as tmp:
        os.environ["MAGICIAN_ROOT_DIR"] = tmp
        global runtime_root  # noqa: PLW0603 - test-scoped override
        original_runtime_root = runtime_root
        runtime_root = lambda: Path(tmp)  # noqa: E731
        try:
            client = SimpleNamespace(principal="anonymous")
            seed_scope_agent_definition(client, "tau2-x", ["calculate", "get_order_details"])
            seeded = (Path(tmp) / "scopes/anonymous/tau2-x/agent_runtime/agents/personal-assistant/definition.agent.yaml").read_text()
            assert "- calculate\n- get_order_details\n" in seeded
            # never overwrites an existing scope definition
            (Path(tmp) / "scopes/anonymous/tau2-y/agent_runtime/agents/personal-assistant").mkdir(parents=True)
            existing = Path(tmp) / "scopes/anonymous/tau2-y/agent_runtime/agents/personal-assistant/definition.agent.yaml"
            existing.write_text("tools:\n- only\n")
            seed_scope_agent_definition(client, "tau2-y", ["calculate"])
            assert existing.read_text() == "tools:\n- only\n"
        finally:
            runtime_root = original_runtime_root
            del os.environ["MAGICIAN_ROOT_DIR"]

    # --- adapter state machine: reflect executed calls, return stored text ---
    bridge2 = ToolBridge()
    bridge2.env = env

    class FakeAgent(MagicianTauAgent):
        def __init__(self, tools, policy, replies, bridge):  # noqa: D107 - no network at all
            self._init_state(SimpleNamespace(args=args, cost_by_workspace={}, profiles_seen=set(), agents=[], bridge=bridge), "selftest", "tau2-selftest")
            self.session_id = "fake"
            self._client = None
            self._replies = list(replies)

        def _send(self, text: str) -> str:
            self.turns += 1
            reply = self._replies.pop(0)
            if reply.startswith("TOOL:"):
                # simulate Magician calling the mounted skill mid-turn
                name = reply.split(":")[1]
                arguments_json = reply.split(":", 2)[2]
                self.context.bridge.execute(name.strip(), json.loads(arguments_json))
                return self._replies.pop(0)
            return reply

    agent = FakeAgent([tool], "Be helpful.", [
        "TOOL:find_user_id_by_name_zip:{\"first_name\": \"Yusuf\"}",
        "You're verified, Yusuf — what would you like to do?",
        "Goodbye!",
    ], bridge2)
    message, _state = agent.generate_next_message(UserMessage.text("I'm Yusuf Rossi."), None)
    assert message.tool_calls and message.tool_calls[0].name == "find_user_id_by_name_zip"
    assert agent._pending_text and "verified" in agent._pending_text
    # orchestrator replays: adapter returns the stored text without a new model call
    turns_before = agent.turns
    results = MultiToolMessage(role="tool", tool_messages=[ToolMessage(id="call_0", role="tool", content="{}", requestor="assistant", error=False)])
    message, _state = agent.generate_next_message(results, None)
    assert isinstance(message, AssistantMessage) and not message.tool_calls and "verified" in (message.content or "")
    assert agent.turns == turns_before, "replay step must not call the model"
    # a no-tool turn is a single plain message
    message, _state = agent.generate_next_message(UserMessage.text("Thanks, bye."), None)
    assert not message.tool_calls and message.content
    bridge2.stop()

    # --- HITL relay: the agent's input-tool questions reach the customer ----
    class FakeRelayClient:
        """Scripted server whose chat turn pauses on input-tool questions:
        each POSTed message opens a turn that asks `questions` one at a
        time (the next one after each answer) and then answers `final`.
        The first turn calls `on_first_post` (a bridge execution) before
        its first question. Every call lands in the shared `log`."""

        def __init__(self, questions: list[str], final: str, log: list[tuple[str, str]], on_first_post: Callable[[], Any] | None = None):
            self.script = list(questions)
            self.final = final
            self.log = log
            self.on_first_post = on_first_post
            self.lock = threading.Lock()
            self.posts: list[str] = []
            self.responds: list[tuple[str, dict[str, Any]]] = []
            self.messages: list[dict[str, Any]] = []
            self.pending: dict[str, Any] | None = None
            self.done = False
            self.turn_id = ""
            self.cancels = 0
            self.requests_made = 0

        def _advance(self) -> None:
            if self.script:
                question = self.script.pop(0)
                self.requests_made += 1
                self.pending = {
                    "id": f"req-{self.requests_made}", "request_type": "need_user_input", "question": question,
                    "options": [], "workspace": "tau2-selftest", "created_at": self.requests_made,
                    "context": {"input_type": "text", "input_schema": {"question": question},
                                "chat_session_id": "fake", "chat_turn_id": self.turn_id},
                }
            else:
                self.pending, self.done = None, True
                self.messages.append({"id": f"a{len(self.messages)}", "chat_turn_id": self.turn_id, "direction": "assistant",
                                      "content": {"text": self.final}, "created_at": 1000 + len(self.messages)})

        def finish_elsewhere(self) -> None:
            """The pending question vanishes and the turn ends on its own."""
            with self.lock:
                self.script = []
                self._advance()

        def json(self, method: str, path: str, body: Any = None, query: Any = None, expected: Any = (200,), timeout: float = 120.0) -> Any:
            self.log.append((method, path))
            with self.lock:
                if method == "POST" and path.endswith("/messages"):
                    self.posts.append(body["text"])
                    self.turn_id, self.done = body["chat_turn_id"], False
                    self.messages.append({"id": f"u{len(self.messages)}", "chat_turn_id": self.turn_id, "direction": "user",
                                          "content": {"text": body["text"]}, "created_at": 1000 + len(self.messages)})
                    if self.on_first_post is not None:
                        self.on_first_post()
                        self.on_first_post = None
                    self._advance()
                    return {"queued": True}
                if method == "GET" and path.endswith("/user-requests"):
                    return {"requests": [self.pending] if self.pending else []}
                if method == "POST" and "/hitl/" in path:
                    request_id = path.split("/")[-2]
                    if self.pending is None or self.pending["id"] != request_id:
                        raise EvalFailure(f"POST {path} -> HTTP 409: already_resolved")
                    assert body["source"] == "user_request" and body["channel"] == "web", body
                    self.responds.append((request_id, body["value"]))
                    self._advance()
                    return {"accepted": True}
                if method == "GET" and path.endswith("/messages"):
                    return {"messages": list(self.messages), "has_more": False}
                if method == "GET" and path.endswith("/events"):
                    turn = path.split("/")[-2]
                    return {"total": len(self.responds), "events": [{"event_type": "LLMResponseReceived", "data": {"timestamp": 1500, "turn": turn}}],
                            "usage": {"cost_usd": 0.1}}
            raise AssertionError(f"unexpected {method} {path}")

        def request(self, method: str, path: str, body: Any = None, query: Any = None, timeout: float = 120.0) -> tuple[int, bytes]:
            assert method == "DELETE" and path.endswith("/run"), (method, path)
            self.log.append((method, path))
            self.cancels += 1
            with self.lock:
                self.pending = None
            return 200, b"{}"

    relay_args = argparse.Namespace(**vars(args))
    relay_args.turn_poll_seconds = 0.01
    relay_args.stall_seconds = 5.0
    relay_args.turn_max_seconds = 0

    def relay_agent(client: Any, bridge: ToolBridge | None) -> MagicianTauAgent:
        made = MagicianTauAgent.__new__(MagicianTauAgent)
        made._init_state(SimpleNamespace(args=relay_args, cost_by_workspace={}, profiles_seen=set(), agents=[], bridge=bridge), "relay-1", "tau2-selftest")
        made.session_id, made._client = "fake", client
        return made

    log: list[tuple[str, str]] = []
    bridge3 = ToolBridge()
    bridge3.env = env
    relay = FakeRelayClient(
        ["What is the email on your account?", "Which order should I exchange?"], "Exchange done.", log,
        on_first_post=lambda: bridge3.execute("get_order_details", {"order_id": "W9"}),
    )
    relay_agent_1 = relay_agent(relay, bridge3)
    # The tool call made before the question is reflected first …
    message, _state = relay_agent_1.generate_next_message(UserMessage.text("I want to exchange a keyboard."), None)
    assert message.tool_calls and message.tool_calls[0].name == "get_order_details", message
    # … then the replay step hands the customer the agent's question.
    message, _state = relay_agent_1.generate_next_message(
        MultiToolMessage(role="tool", tool_messages=[ToolMessage(id="call_0", role="tool", content="{}", requestor="assistant", error=False)]), None)
    assert message.content == "What is the email on your account?" and not message.tool_calls, message
    assert relay.posts == ["CUSTOMER: I want to exchange a keyboard."] and relay.responds == []
    # The customer's reply is the HITL answer, not a new chat message; the
    # same turn then asks again (a second question in one turn) …
    message, _state = relay_agent_1.generate_next_message(UserMessage.text("yusuf@example.com"), None)
    assert relay.responds == [("req-1", {"type": "text", "value": "yusuf@example.com"})], relay.responds
    assert message.content == "Which order should I exchange?" and len(relay.posts) == 1
    # … and finishes once that one is answered too.
    message, _state = relay_agent_1.generate_next_message(UserMessage.text("Order W9."), None)
    assert relay.responds[-1] == ("req-2", {"type": "text", "value": "Order W9."}) and len(relay.responds) == 2
    assert message.content == "Exchange done." and len(relay.posts) == 1
    assert relay_agent_1.hitl_questions_relayed == 2 and relay_agent_1.turns == 1 and relay_agent_1._relay is None
    assert relay_agent_1.tool_calls_made == 1

    # A question that vanishes (turn finished on its own) falls back to an
    # ordinary chat message carrying the customer's words.
    vanishing = FakeRelayClient(["Your email?"], "Anything else?", [])
    relay_agent_2 = relay_agent(vanishing, None)
    relay_agent_2.context.bridge = ToolBridge()
    relay_agent_2.context.bridge.env = env
    message, _state = relay_agent_2.generate_next_message(UserMessage.text("Hi."), None)
    assert message.content == "Your email?" and relay_agent_2.hitl_questions_relayed == 1
    vanishing.finish_elsewhere()
    message, _state = relay_agent_2.generate_next_message(UserMessage.text("me@example.com"), None)
    assert vanishing.responds == [] and vanishing.posts == ["CUSTOMER: Hi.", "CUSTOMER: me@example.com"], vanishing.posts
    assert message.content == "Anything else?" and relay_agent_2.turns == 2
    relay_agent_2.context.bridge.stop()

    # --no-hitl-relay: the probe is off, the turn is waited on as before.
    plain = FakeRelayClient([], "Hello!", [])
    plain_args = argparse.Namespace(**vars(relay_args))
    plain_args.hitl_relay = False
    relay_agent_3 = relay_agent(plain, None)
    relay_agent_3.context.args = plain_args
    assert relay_agent_3._send("CUSTOMER: hi") == "Hello!"
    assert not any(path.endswith("/user-requests") for _method, path in plain.log)

    # --- HITL answerer: confirmations/choices still auto-answered; free text
    # is left to the relay (or canned, truthful text with the relay off) ----
    class FakeHitlClient:
        def __init__(self, requests: list[dict[str, Any]]):
            self.requests = requests
            self.responds: dict[str, dict[str, Any]] = {}

        def json(self, method: str, path: str, body: Any = None, query: Any = None, expected: Any = (200,), timeout: float = 120.0) -> Any:
            if path.endswith("/auth/session/scope"):
                return {"token": "scoped"}
            if path.endswith("/user-requests"):
                return {"requests": self.requests}
            if "/hitl/" in path:
                self.responds[path.split("/")[-2]] = body["value"]
                return {"accepted": True}
            raise AssertionError(f"unexpected {method} {path}")

    def hitl_requests() -> list[dict[str, Any]]:
        return [
            {"id": "confirm-1", "request_type": "need_user_input", "question": "Proceed with the exchange?", "workspace": "tau2-w",
             "options": [], "context": {"input_type": "confirmation", "input_schema": {"input_type": "confirmation"}}},
            {"id": "text-1", "request_type": "need_user_input", "question": "What is your zip code?", "workspace": "tau2-w",
             "options": [], "context": {"input_type": "text", "input_schema": {"question": "What is your zip code?"}}},
            {"id": "choice-1", "request_type": "tool_authorization", "question": "Allow the tool?", "workspace": "tau2-w",
             "options": [{"id": "allow_once", "label": "Allow once"}, {"id": "deny", "label": "Deny"}],
             "context": {"input_schema": {"input_type": "choice"}}},
        ]

    for relay_on in (True, False):
        fake_hitl = FakeHitlClient(hitl_requests())
        answerer = HitlAnswerer(lambda fake=fake_hitl: fake, relay_free_text=relay_on)
        answerer.set_workspace("tau2-w")
        answerer._tick()
        assert fake_hitl.responds["confirm-1"] == {"type": "confirmation", "confirmed": True}, fake_hitl.responds
        assert fake_hitl.responds["choice-1"]["type"] == "choice", fake_hitl.responds
        if relay_on:
            assert "text-1" not in fake_hitl.responds, "the relay owns free-text questions"
        else:
            assert fake_hitl.responds["text-1"] == {"type": "text", "value": HitlAnswerer.CANNED_TEXT}
    assert "wire protocol" not in HitlAnswerer.CANNED_TEXT.lower() and "chat replies" in HitlAnswerer.CANNED_TEXT

    # --- transcript export runs BEFORE the purge; a kept workspace is not
    # purged; a failed export only warns -----------------------------------
    class FakeDriverClient:
        principal = ""

        def __init__(self, log: list[tuple[str, str]]):
            self.log = log

        def json(self, method: str, path: str, body: Any = None, query: Any = None, expected: Any = (200,), timeout: float = 120.0) -> Any:
            assert method == "DELETE" and "/workspaces/" in path, (method, path)
            self.log.append((method, path))
            return {}

    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp)
        log.clear()
        finished = finish_task_workspace(FakeDriverClient(log), relay_agent_1, out, keep_workspace=False)  # type: ignore[arg-type]
        transcript = Path(finished["transcript"])
        assert transcript.parent == out / "transcripts" and transcript.name.startswith("relay-1-tau2-selftest"), transcript
        exported = json.loads(transcript.read_text())
        kinds = [entry["kind"] for entry in exported["entries"]]
        assert {"message", "turn_event", "tool_bridge", "hitl_relay_question", "hitl_relay_answer"} <= set(kinds), kinds
        stamps = [entry["ts"] for entry in exported["entries"] if entry["ts"] is not None]
        assert stamps == sorted(stamps)
        delete_at = log.index(("DELETE", "/api/magician/v2/workspaces/tau2-selftest"))
        last_export = max(i for i, (method, path) in enumerate(log) if method == "GET")
        assert last_export < delete_at, log
        assert finished["hitl_questions_relayed"] == 2 and finished["turns"] == 1
        # A conversation that ends on an unanswered question cancels the
        # paused turn; --keep-workspaces skips the purge, never the export.
        log.clear()
        dangling = FakeRelayClient(["Your name?"], "never", log)
        relay_agent_4 = relay_agent(dangling, None)
        relay_agent_4.context.bridge = ToolBridge()
        relay_agent_4.context.bridge.env = env
        message, _state = relay_agent_4.generate_next_message(UserMessage.text("Hello"), None)
        assert message.content == "Your name?"
        relay_agent_4.task_id = "relay-4"
        kept = finish_task_workspace(FakeDriverClient(log), relay_agent_4, out, keep_workspace=True)  # type: ignore[arg-type]
        assert dangling.cancels == 1 and kept.get("workspace_kept") and Path(kept["transcript"]).is_file()
        assert not any(method == "DELETE" and "/workspaces/" in path for method, path in log)
        relay_agent_4.context.bridge.stop()
        # Export failure warns; the purge still happens.
        broken = relay_agent(None, None)
        broken.task_id = "relay-5"
        log.clear()
        failed = finish_task_workspace(FakeDriverClient(log), broken, out, keep_workspace=False)  # type: ignore[arg-type]
        assert "transcript" not in failed and log and log[-1][0] == "DELETE"
    bridge3.stop()

    # --- report writing -------------------------------------------------------
    report_path = write_report(
        Path(args.output_dir) / "self-test", args, {"self_test": True},
        [{"task_id": "t1", "reward": 1.0, "cost_usd": 0.01, "turns": 4, "tool_calls_made": 3, "wall_seconds": 12.0, "seed": 1},
         {"task_id": "t2", "reward": 0.0, "cost_usd": 0.02, "turns": 5, "tool_calls_made": 2, "wall_seconds": 14.0, "seed": 2}],
        "completed", set(),
    )
    payload = json.loads(report_path.read_text())
    assert payload["verdict"] == "pass" and payload["summary"]["pass_at_1"] == 0.5
    print(f"self-test: OK (report at {report_path})")
    return 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base-url", default=os.environ.get("MAGICIAN_API", DEFAULT_BASE_URL))
    parser.add_argument("--bearer", default="")
    parser.add_argument("--domain", default="retail", help="tau2 domain: retail | airline | telecom | mock")
    parser.add_argument("--limit", type=int, default=DEFAULT_LIMIT)
    parser.add_argument("--task-ids", default="", help="comma-separated task ids (overrides --limit)")
    parser.add_argument("--seed", type=int, default=42)
    add_chat_model_arguments(parser, ("answer",))
    parser.add_argument("--user-llm", default="", help="LLM name for tau2's user simulator (default: tau2's default)")
    parser.add_argument("--max-cost-usd", type=float, default=DEFAULT_MAX_COST_USD)
    parser.add_argument("--stall-seconds", type=float, default=DEFAULT_STALL_SECONDS, help="give up on a turn only after this long with no new turn event and no tool execution")
    parser.add_argument("--turn-max-seconds", type=float, default=DEFAULT_TURN_MAX_SECONDS, help="wall-clock backstop for a turn that is still progressing (0 = none)")
    parser.add_argument("--hitl-relay", action=argparse.BooleanOptionalAction, default=True, help="relay the agent's free-text HITL questions to the user simulator and post its reply as the answer (--no-hitl-relay: auto-answer them with canned text, the old behavior)")
    parser.add_argument("--keep-workspaces", action="store_true", help="do not purge task workspaces after export (inspect them afterwards)")
    parser.add_argument("--output-dir", default=str(DEFAULT_OUTPUT_DIR))
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return self_test(args)
    return run(args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeUnavailable as error:
        print(f"ABORTED (runtime unavailable): {error}", file=sys.stderr)
        sys.exit(2)
    except EvalFailure as error:
        print(f"FAILED: {error}", file=sys.stderr)
        sys.exit(2)

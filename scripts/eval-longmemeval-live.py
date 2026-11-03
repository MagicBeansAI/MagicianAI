#!/usr/bin/env python3
"""LongMemEval through the real Magician chat+memory path.

The benchmark (Wu et al., ICLR 2025; HF dataset ``xiaowu0162/longmemeval``)
gives, per instance, ~30-60 past chat sessions of one synthetic persona
(``haystack_sessions``) and one question whose answer is only derivable from
that history. The score is LLM-judge accuracy over 500 instances (S variant).

This driver runs Magician AS THE PRODUCT: every haystack session is ingested
through the public chat API inside a fresh workspace (memory pipeline fully
on), then the question is asked in a brand-new session so only durable memory
— not in-session context — can carry the answer.

Two ingestion protocols (``--ingest``):

  digest  one transcript message per haystack session. This is the standard
          memory-agent protocol the published baselines use (Mem0 et al. call
          ``memory.add(session_messages)``), so numbers are comparable, and it
          costs ~50 turns per instance instead of ~500.
  lived   turn-by-turn replay: every dataset turn becomes one real chat turn
          (dataset assistant turns ride the next user message as a bracketed
          quote, because the API only accepts user messages). The premium
          product path; ~10x the turns and cost of digest.

Judging uses the benchmark's own answer-check prompts (per question type,
verbatim from ``src/evaluation/evaluate_qa.py`` in the LongMemEval repo),
executed as one-shot chat turns in a FRESH judge workspace per instance —
the judge stays stateless, and instance memory can never bias a verdict. EM
and SQuAD-style token F1 are reported alongside the judge accuracy.

Isolation: fresh workspace per instance (memory is principal+workspace
scoped), purged after the instance unless ``--keep-workspaces``. Each
instance record carries the ledger cost of its turns, split into ingest /
question phases, so a report can always price its own accuracy.

Live mode needs a running runtime with ``MAGICIAN_EVAL_USERNAME`` /
``MAGICIAN_EVAL_PASSWORD`` (env, or ``$MAGICIAN_ROOT_DIR/.env.development``
then ``.env``) or ``MAGICIAN_BEARER_TOKEN``/``--bearer``. ``--self-test`` is
provider-free and exercises fixture ingestion, judge selection, metrics and
the report against a fake client.

A server that goes away mid-run (restart, binary replace) aborts the run as
inconclusive — instances never count as wrong answers for an outage.
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
from dataclasses import dataclass, field
from html import escape
from pathlib import Path
from typing import Any

from chat_model_args import add_chat_model_arguments, chat_model_request_fields, resolve_chat_model_selections

print = functools.partial(print, flush=True)  # noqa: A001 - background runs must not buffer progress
REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BASE_URL = "http://127.0.0.1:3002"
DEFAULT_DATASET = Path.home() / ".cache/magician-evals/longmemeval/longmemeval_s"
DEFAULT_OUTPUT_DIR = REPO_ROOT / "coverage/evals/longmemeval/live/latest"
DEFAULT_LIMIT = 5
# The send endpoint rejects text over 16 KiB; stay under with framing room.
MAX_MESSAGE_CHARS = 14_000
# Keep the conservative replay size until complete source capture is deployed.
# Source sessions stay intact even when their transcript needs several sends.
DEFAULT_DIGEST_CHUNK_CHARS = 1_800
MAX_RESPONSE_BYTES = 8 * 1024 * 1024
SEND_TIMEOUT_SECONDS = 600.0
SETTLE_POLL_SECONDS = 5.0
DEFAULT_SETTLE_TIMEOUT_SECONDS = 180.0
DEFAULT_SETTLE_EXTRA_SECONDS = 45.0
DEFAULT_MAX_COST_USD = 25.0
WORKSPACE_SLUG_PREFIX = "lmeval"
# Written into every harness-created scope dir; the offline sweep script
# keys on it (and on registry-orphan status for pre-marker runs).
WORKSPACE_MARKER_FILE = ".longmemeval-eval"
# Static reference points for the report, each tied to its public source.
# Numbers on record for LongMemEval_S / LongMemEval (judge accuracy);
# refreshed deliberately — never extend this table from memory.
BASELINES_S = [
    {
        "system": "GPT-4o + full haystack in context (LongMemEval paper, S)",
        "score": "64.0%",
        "source": "arXiv:2410.10813 (Table 3, Chain-of-Note + JSON)",
    },
    {
        "system": "GPT-4o-mini + full long-context window (no memory system)",
        "score": "~57.6-57.9%",
        "source": "Zep blog 'Is OpenAI Overselling Long-Context?' (2025-04); benchd.ai LLM baseline",
    },
    {
        "system": "MemGAS full-history baseline, GPT-4o-mini answering",
        "score": "50.6%",
        "source": "MemGAS paper (2025)",
    },
    {
        "system": "Zep memory system, GPT-4o-mini answering (LongMemEval-S)",
        "score": "71.2%",
        "source": "getzep.com LongMemEval analysis (2025-04)",
    },
    {
        "system": "Recent memory systems (Mem0/Mi-Memory/ActMem etc., self-reported)",
        "score": "66-90% (protocol-dependent)",
        "source": "respective papers/blogs 2025-2026; judge and protocol variance is high",
    },
]


class EvalFailure(RuntimeError):
    pass


class RuntimeUnavailable(EvalFailure):
    """The runtime went away mid-run (restart, binary replace). The run is
    inconclusive — an outage must never be scored as wrong answers."""


# ---------------------------------------------------------------------------
# HTTP client (mirrors scripts/eval-harness-conformance-live.py)
# ---------------------------------------------------------------------------


class Client:
    def __init__(self, base_url: str, bearer: str | None):
        self.base_url = base_url.rstrip("/")
        self.bearer = bearer

    def request(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        timeout: float = 120.0,
    ) -> tuple[int, bytes]:
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
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            raise RuntimeUnavailable(f"{method} {path}: {error}") from error
        if len(raw) > MAX_RESPONSE_BYTES:
            raise EvalFailure(f"response exceeded {MAX_RESPONSE_BYTES} bytes: {path}")
        return status, raw

    def json(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
        expected: tuple[int, ...] = (200,),
        timeout: float = 120.0,
    ) -> Any:
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


# ---------------------------------------------------------------------------
# Auth: --bearer > MAGICIAN_BEARER_TOKEN > eval login (env, .env.development, .env)
# ---------------------------------------------------------------------------


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


def login_client(args: argparse.Namespace) -> Client:
    """A fresh login session (independent live token). Used for the main
    driver and, separately, for the HITL answerer below."""
    root = Path(os.environ.get("MAGICIAN_ROOT_DIR", str(Path.home() / "MagicianNotes")))
    username = read_eval_credential("MAGICIAN_EVAL_USERNAME", root)
    password = read_eval_credential("MAGICIAN_EVAL_PASSWORD", root)
    if not username or not password:
        raise EvalFailure(
            "login needs MAGICIAN_EVAL_USERNAME/MAGICIAN_EVAL_PASSWORD "
            f"(env or {root}/.env.development)"
        )
    client = Client(args.base_url, None)
    login = client.json(
        "POST", "/api/magician/v2/auth/login",
        {"username": username, "password": password}, expected=(200, 201),
    )
    client.bearer = login["token"]
    return client


def bootstrap_client(args: argparse.Namespace) -> tuple[Client, dict[str, Any]]:
    """Return a client authenticated against the login scope (default workspace).

    The caller immediately rotates onto a benchmark workspace; a token minted
    here is only valid until the first rotation (the server keeps one live
    session per login and rotates it in place).
    """
    bearer = args.bearer or os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
    if not bearer:
        client = login_client(args)
    else:
        client = Client(args.base_url, bearer)
    session = client.json("GET", "/api/magician/v2/auth/session")
    client.principal = str(session.get("principal") or "")
    return client, session


def scope_client(client: Client, workspace: str) -> Client:
    """Rotate the session onto ``workspace``, in place. The server keeps one
    live session per login, so every prior token — including this client's —
    dies here; the caller must not keep a second client around."""
    rotated = client.json(
        "POST", "/api/magician/v2/auth/session/scope", {"workspace": workspace}
    )
    client.bearer = rotated["token"]
    return client


# ---------------------------------------------------------------------------
# HITL answerer
# ---------------------------------------------------------------------------


class HitlAnswerer:
    """Answers ``need_user_input`` requests on an independent login session.

    Chat turns block on a user request for up to 24h, and a benchmark has no
    human: the first pilot found a haystack transcript (a river-crossing
    puzzle) that made the assistant dispatch a specialist, fail, and ask how
    to proceed. The answerer polls the current workspace and responds within
    seconds; every interruption it answers is recorded on the instance so the
    report can show how often the assistant stopped to ask."""

    POLL_SECONDS = 2.0
    CANNED_TEXT = (
        "No user input is available in this benchmark session. Proceed on "
        "your own judgement; do not start tasks, and do not ask again."
    )
    # Memory-write confirmations must be answered YES — a skipped save is a
    # lost haystack fact and a silently wrong benchmark score. Action
    # questions get the minimal-action option instead.
    _SAVE_WORDS = ("save", "yes", "remember", "keep", "store", "summar", "memor", "confirm", "allow", "record")
    _MINIMAL_ACTION_WORDS = ("skip", "none", "continue", "ignore", "not needed", "no action", "proceed", "dismiss")
    _MEMORY_QUESTION_WORDS = ("memory", "save", "summary", "remember", "note")

    def __init__(self, client_factory):
        self._client_factory = client_factory
        self._client: Client | None = None
        self._scoped_workspace = ""
        self._workspace = ""
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self.answered: list[dict[str, Any]] = []
        self._failed_attempts: dict[str, int] = {}
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, daemon=True, name="hitl-answerer")
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=5.0)

    def set_workspace(self, slug: str) -> None:
        with self._lock:
            self._workspace = slug

    def snapshot_and_clear(self) -> list[dict[str, Any]]:
        with self._lock:
            snapshot = list(self.answered)
            self.answered = []
        return snapshot

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                self._tick()
            except Exception as error:  # noqa: BLE001 - the answerer must survive anything
                # A dead session (server restart) re-logins on the next tick.
                if isinstance(error, (RuntimeUnavailable, EvalFailure)):
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
            self._answer(request)

    def _answer(self, request: dict[str, Any]) -> None:
        request_id = request.get("id") or ""
        if self._failed_attempts.get(request_id, 0) >= 3:
            return  # a request we cannot answer must not spam the endpoint every tick
        question = str(request.get("question") or "")[:200]
        schema = request.get("input_schema") or {}
        options = request.get("options") or schema.get("options") or []
        input_type = schema.get("input_type") or request.get("input_type") or "choice"
        if input_type == "choice" and options:
            question_lower = question.lower()
            wants_save = any(word in question_lower for word in self._MEMORY_QUESTION_WORDS)
            preference_words = self._SAVE_WORDS if wants_save else self._MINIMAL_ACTION_WORDS
            preferred = next(
                (
                    option
                    for option in options
                    if any(word in f"{option.get('id', '')} {option.get('label', '')}".lower() for word in preference_words)
                ),
                None,
            )
            chosen = preferred or options[0]
            value: dict[str, Any] = {"type": "choice", "selected_id": chosen.get("id")}
            answer_summary = f"choice:{chosen.get('id')}"
        else:
            # AgenticResumeValue::Text is tagged {"type","value"} — a "text"
            # field here 400s invalid_request_payload and the turn stays
            # blocked (verified live against the canonical respond endpoint).
            value = {"type": "text", "value": self.CANNED_TEXT}
            answer_summary = "text"
        record = {"question": question, "answered_with": answer_summary}
        try:
            self._client.json(  # type: ignore[union-attr]
                "POST",
                f"/api/magician/v2/hitl/{request_id}/respond",
                {"source": "user_request", "value": value},
                expected=(200, 201, 202, 204),
            )
            with self._lock:
                self.answered.append(record)
            print(f"  [hitl] answered ({answer_summary}): {question[:100]}")
        except EvalFailure as error:
            self._failed_attempts[request_id] = self._failed_attempts.get(request_id, 0) + 1
            print(f"  [hitl] FAILED to answer request {request_id}: {error}")


# ---------------------------------------------------------------------------
# Chat helpers
# ---------------------------------------------------------------------------


def new_chat_session(client: Client, ui_thread_id: str, attempts: int = 3) -> str:
    """Create a chat session, retrying transient failures (a loaded server
    answered HTTP 408 here while two eval processes were running)."""
    last_error: Exception | None = None
    for attempt in range(attempts):
        try:
            payload = client.json("POST", "/api/magician/v2/chat/new", {}, query={"ui_thread_id": ui_thread_id})
            session = payload.get("session") if isinstance(payload, dict) else None
            session_id = (session or {}).get("id") or payload.get("id") if isinstance(payload, dict) else None
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
) -> dict[str, Any]:
    """One synchronous chat turn. Returns the full ChatResponse dict.

    A send that lands while the previous turn is still running returns
    ``queued`` with no assistant message; the turn is then awaited by polling
    the session history for our ``chat_turn_id`` (usage for such turns is not
    recoverable from history, so their cost is under-counted — recorded, not
    hidden)."""
    chat_turn_id = f"bench-{secrets.token_hex(8)}"
    body: dict[str, Any] = {"text": text, "source_surface": "web", "chat_turn_id": chat_turn_id}
    body.update(chat_model_request_fields(model_selection))
    last_error: Exception | None = None
    for attempt in range(retries + 1):
        try:
            response = client.json(
                "POST",
                f"/api/magician/v2/chat/sessions/{session_id}/messages",
                body,
                timeout=SEND_TIMEOUT_SECONDS,
            )
            if isinstance(response, dict) and response.get("assistant_message"):
                return response
            if isinstance(response, dict) and (response.get("queued") or response.get("cancelled")):
                if response.get("cancelled"):
                    raise EvalFailure(f"turn {chat_turn_id} was cancelled server-side")
                return await_turn_by_id(client, session_id, chat_turn_id)
            raise EvalFailure(f"turn {chat_turn_id} returned no assistant message: {json.dumps(response)[:200]}")
        except RuntimeUnavailable as error:
            # A restart mid-run kills every outstanding token too; the next
            # retry distinguishes "blip" from "runtime gone".
            last_error = error
            if attempt < retries:
                time.sleep(3.0 * (attempt + 1))
        except EvalFailure as error:
            message = str(error)
            # 5xx turns are usually one malformed model tool-call decision
            # (seen live: arguments_do_not_match_schema) — a resample usually
            # clears it, so retry like the transient cases. 408 is the same
            # server-busy timeout chat/new retries on (it killed instance 3
            # of the confirmation run at its first turn before this was
            # added).
            transient = (
                "-> HTTP 401" in message
                or "-> HTTP 408" in message
                or "no assistant message" in message
                or "-> HTTP 5" in message
            )
            if transient and attempt < retries:
                last_error = error
                time.sleep(2.0)
                continue
            raise
    raise RuntimeUnavailable(f"send_message exhausted retries: {last_error}")


def await_turn_by_id(client: Client, session_id: str, chat_turn_id: str) -> dict[str, Any]:
    deadline = time.monotonic() + SEND_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        history = client.json(
            "GET", f"/api/magician/v2/chat/sessions/{session_id}/messages", query={"limit": 50}
        )
        for message in reversed((history or {}).get("messages") or []):
            if message.get("chat_turn_id") == chat_turn_id and message.get("direction") == "assistant":
                return {"assistant_message": message, "usage": {}}
        time.sleep(2.0)
    raise EvalFailure(f"queued turn {chat_turn_id} never completed")


def assistant_text(response: dict[str, Any]) -> str:
    message = response.get("assistant_message") or {}
    content = message.get("content") or {}
    text = content.get("text")
    if not text:
        presentation = message.get("presentation") or {}
        text = presentation.get("plain_text") or ""
    return str(text)


def usage_cost(response: dict[str, Any]) -> tuple[float, int, int]:
    usage = response.get("usage") or {}
    cost = usage.get("costUsd")
    return (float(cost) if isinstance(cost, (int, float)) else 0.0,
            int(usage.get("inputTokens") or 0),
            int(usage.get("outputTokens") or 0))


def usage_profile(response: dict[str, Any]) -> str:
    usage = response.get("usage") or {}
    return str(usage.get("profile") or "")


def local_runtime_root(client: Client) -> Path | None:
    """The served runtime root when this machine can see it: only for a
    loopback base URL (a remote runtime keeps its scopes on another disk, so
    its index cannot be checked from here, whatever this shell exports), then
    MAGICIAN_ROOT_DIR, MAGICIAN_STORAGE_PATH (the order run-supervisor.sh
    resolves), else the default ~/MagicianNotes."""
    host = (urllib.parse.urlparse(client.base_url).hostname or "").lower()
    if host not in ("localhost", "127.0.0.1", "::1"):
        return None
    for name in ("MAGICIAN_ROOT_DIR", "MAGICIAN_STORAGE_PATH"):
        root = os.environ.get(name, "").strip()
        if root:
            return Path(root)
    return Path.home() / "MagicianNotes"


def index_paused_for_rebuild(root: Path, principal: str, slug: str) -> bool:
    """The maintainer paused the scope until an explicit rebuild
    (rebuild-required.json): its journal will not drain, so stop waiting."""
    return (root / "scopes" / principal / slug / "memory" / "index" / "rebuild-required.json").is_file()


def index_covers_writes(root: Path, principal: str, slug: str) -> bool:
    """The scope's derived memory index exists AND has applied every journaled
    canonical write — the signal that hybrid retrieval sees the whole haystack.
    Found the hard way: a scope whose index has not been built serves
    question-time retrieval from the lexical fast stage ALONE
    (retrieval_backends: ["fast_snapshot"]), which misses anything not near a
    turn's head — the fast-lane flip's root cause. A manifest alone is not
    enough: the first build lands about a minute into ingest, and the later
    sessions (often holding the gold answer) wait in changes.json until the
    next build applies them. A memory write that lands after ingest (extraction,
    consolidation) re-dirties the journal and extends the wait until the next
    build applies it, bounded by --index-wait-timeout."""
    index_dir = root / "scopes" / principal / slug / "memory" / "index"
    try:
        # Same test as the maintainer's cold start: an empty manifest
        # (document_count 0) still serves lexical-only retrieval.
        if not json.loads((index_dir / "manifest.json").read_text()).get("document_count"):
            return False
    except (OSError, ValueError, AttributeError):
        return False
    try:
        journal = json.loads((index_dir / "changes.json").read_text())
    except FileNotFoundError:
        return True
    except (OSError, ValueError):
        return False
    return not (journal or {}).get("entries")


def memory_search(client: Client, query: str) -> int:
    payload = client.json("POST", "/api/magician/v2/chat/memory/search", {"query": query})
    return len((payload or {}).get("results") or [])


def create_workspace(client: Client, slug: str, display_name: str) -> None:
    client.json(
        "POST", "/api/magician/v2/workspaces",
        {"slug": slug, "display_name": display_name, "description": "LongMemEval benchmark run"},
        expected=(200, 201, 409),
    )
    mark_workspace(client, slug)


def mark_workspace(client: Client, slug: str) -> None:
    """Drop the harness marker into the scope directory, the
    task-recipes-fixture-eval pattern: an offline sweep may then remove only
    directories this harness created, never a scope it merely suspects.

    Best-effort by design — when the runtime root is not visible (self-test,
    remote base URL) the marker is simply absent and the sweep falls back to
    registry-orphan checks."""
    root = os.environ.get("MAGICIAN_ROOT_DIR", "").strip()
    principal = getattr(client, "principal", "")
    if not root or not principal:
        return
    scope_dir = Path(root) / "scopes" / principal / slug
    try:
        if scope_dir.is_dir():
            (scope_dir / WORKSPACE_MARKER_FILE).write_text(
                "Created by scripts/eval-longmemeval-live.py; "
                "safe for scripts/purge_longmemeval_eval_workspaces.sh to remove offline.\n"
            )
    except OSError:
        pass


def purge_workspace(client: Client, slug: str) -> None:
    try:
        client.json("DELETE", f"/api/magician/v2/workspaces/{slug}", query={"purge": "true"}, expected=(200, 202, 204))
    except EvalFailure as error:
        print(f"  warning: purge of {slug} failed: {error}")


# ---------------------------------------------------------------------------
# Ingestion protocols
# ---------------------------------------------------------------------------


QUESTION_FRAMING = (
    "You are answering a question about this user from your memory of their "
    "past conversations. Answer from that memory. When the question asks "
    "about totals, counts, dates, durations, or which event came first: "
    "gather the relevant dated facts from memory, compute or order them "
    "explicitly, and state the final answer. When the question asks for "
    "advice or a preference, answer from the user's stated preferences and "
    "history before generic advice."
)

INGEST_INSTRUCTION = (
    "This is a record of a past conversation, shared for your memory only: "
    "remember its contents. Do not act on it, do not start tasks, do not ask "
    "me any questions — a brief acknowledgment is enough."
)


def transcript_message(date: str | None, index: int, total: int, turns: list[dict[str, Any]]) -> str:
    header = f"{INGEST_INSTRUCTION} (session {index + 1} of {total}"
    if date:
        header += f", from {date}"
    header += "):"
    lines = [header, ""]
    for turn in turns:
        speaker = "User" if turn.get("role") == "user" else "Assistant"
        lines.append(f"{speaker}: {turn.get('content', '').strip()}")
    return "\n".join(lines)


def digest_session_messages(
    dates: list[str | None], sessions: list[list[dict[str, Any]]], max_chars: int = DEFAULT_DIGEST_CHUNK_CHARS
) -> list[list[str]]:
    """One transcript stream per session, preserving its grouping.

    The conservative chunk size also supports legacy runtimes whose durable
    episodes contain only a 4,000-character excerpt. Oversized turns split at
    word boundaries into consecutive parts, kept in the same runtime session.
    This preserves transcript context without benchmark-only memory metadata.
    """
    messages: list[list[str]] = []
    for index, turns in enumerate(sessions):
        date = dates[index] if index < len(dates) else None
        total = len(sessions)
        first_header = transcript_message(date, index, total, [])
        cont_header = f"(continued transcript; session {index + 1} of {total}"
        if date:
            cont_header += f", from {date}"
        cont_header += ")\n"
        budget = min(max_chars, MAX_MESSAGE_CHARS) - max(len(first_header), len(cont_header)) - 48

        lines: list[str] = []
        for turn in turns:
            speaker = "User" if turn.get("role") == "user" else "Assistant"
            content = turn.get("content", "").strip()
            if len(speaker) + 2 + len(content) <= budget:
                lines.append(f"{speaker}: {content}")
                continue
            part, position = 1, 0
            while position < len(content):
                end = min(position + budget, len(content))
                if end < len(content):
                    # Prefer a sentence boundary, then any word boundary: a
                    # part-line that starts mid-sentence reads as a truncated
                    # transcript and can trigger a "the transcript ends
                    # mid-sentence" user request mid-ingestion.
                    window = content[position + (budget // 3) : end]
                    cut = max(window.rfind(". "), window.rfind("! "), window.rfind("? "), window.rfind(" "))
                    if cut > 0:
                        end = position + (budget // 3) + cut + 1
                piece = content[position:end].strip()
                if piece:
                    lines.append(f"{speaker} (part {part}): {piece}")
                    part += 1
                position = end

        packed: list[tuple[str, list[str]]] = []
        for line in lines:
            if packed:
                header_now, current = packed[-1]
                if len("\n".join(current)) + len(line) + 1 <= budget:
                    current.append(line)
                    continue
            header_now = first_header if not packed else cont_header
            packed.append((header_now, [line]))
        messages.append([header + "\n" + "\n".join(current) for header, current in packed])
    return messages


def digest_messages(
    dates: list[str | None], sessions: list[list[dict[str, Any]]], max_chars: int = DEFAULT_DIGEST_CHUNK_CHARS
) -> list[str]:
    """Compatibility view for callers that only inspect the rendered sends."""
    return [message for group in digest_session_messages(dates, sessions, max_chars) for message in group]


def lived_messages(sessions: list[list[dict[str, Any]]]) -> list[list[str]]:
    """Every dataset turn becomes one user message. Dataset assistant turns
    ride as a bracketed quote on the message (the API accepts user text
    only, and the benchmark's evidence lives on both sides of the wire).
    Each session opens with the remember-only instruction so the replay
    cannot be mistaken for live requests to act on."""
    per_session: list[list[str]] = []
    for turns in sessions:
        messages: list[str] = [INGEST_INSTRUCTION]
        for turn in turns:
            content = turn.get("content", "").strip()
            if turn.get("role") == "user":
                messages.append(content)
            else:
                messages.append(f'[the assistant had replied]: "{content}"')
        per_session.append(messages)
    return per_session


def distinctive_phrase(text: str) -> str:
    """A lowercase probe phrase for the memory/search settle check: several
    consecutive mid-length words, unlikely to appear verbatim elsewhere.
    Dataset turn contents are not always strings (ints and other JSON scalars
    appear), so coerce first."""
    text = str(text)
    words = re.findall(r"[a-zA-Z']{4,}", text)
    for index in range(max(1, len(words) - 5)):
        candidate = " ".join(words[index : index + 5]).lower()
        if 24 <= len(candidate) <= 90:
            return candidate
    return " ".join(words[-5:]).lower() if words else "conversation transcript"


# ---------------------------------------------------------------------------
# Judge (prompts verbatim from the LongMemEval repo, src/evaluation/evaluate_qa.py)
# ---------------------------------------------------------------------------

_JUDGE_TEMPLATES: dict[str, str] = {
    "default": (
        "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response "
        "contains the correct answer. Otherwise, answer no. If the response is equivalent to the correct answer or "
        "contains all the intermediate steps to get the correct answer, you should also answer yes. If the response "
        "only contains a subset of the information required by the answer, answer no. \n\nQuestion: {}\n\nCorrect "
        "Answer: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only."
    ),
    "temporal-reasoning": (
        "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response "
        "contains the correct answer. Otherwise, answer no. If the response is equivalent to the correct answer or "
        "contains all the intermediate steps to get the correct answer, you should also answer yes. If the response "
        "only contains a subset of the information required by the answer, answer no. In addition, do not penalize "
        "off-by-one errors for the number of days. If the question asks for the number of days/weeks/months, etc., "
        "and the model makes off-by-one errors (e.g., predicting 19 days when the answer is 18), the model's response "
        "is still correct. \n\nQuestion: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\nIs the model response "
        "correct? Answer yes or no only."
    ),
    "knowledge-update": (
        "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response "
        "contains the correct answer. Otherwise, answer no. If the response contains some previous information along "
        "with an updated answer, the response should be considered as correct as long as the updated answer is the "
        "required answer.\n\nQuestion: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\nIs the model response "
        "correct? Answer yes or no only."
    ),
    "single-session-preference": (
        "I will give you a question, a rubric for desired personalized response, and a response from a model. Please "
        "answer yes if the response satisfies the desired response. Otherwise, answer no. The model does not need to "
        "reflect all the points in the rubric. The response is correct as long as it recalls and utilizes the user's "
        "personal information correctly.\n\nQuestion: {}\n\nRubric: {}\n\nModel Response: {}\n\nIs the model response "
        "correct? Answer yes or no only."
    ),
    "abstention": (
        "I will give you an unanswerable question, an explanation, and a response from a model. Please answer yes if "
        "the model correctly identifies the question as unanswerable. The model could say that the information is "
        "incomplete, or some other information is given but the asked information is not.\n\nQuestion: {}\n\n"
        "Explanation: {}\n\nModel Response: {}\n\nDoes the model correctly identify the question as unanswerable? "
        "Answer yes or no only."
    ),
}

_JUDGE_TYPE_MAP = {
    "single-session-user": "default",
    "single-session-assistant": "default",
    "multi-session": "default",
}


def judge_prompt(question_type: str, question_id: str, question: str, answer: str, response: str) -> str:
    if "_abs" in question_id:
        template = _JUDGE_TEMPLATES["abstention"]
    else:
        template = _JUDGE_TEMPLATES[_JUDGE_TYPE_MAP.get(question_type, question_type)]
    return template.format(str(question), str(answer), str(response))


def judge_label(raw_reply: str) -> bool:
    # Official parse: `label = 'yes' in eval_response.lower()`.
    return "yes" in raw_reply.lower()


# ---------------------------------------------------------------------------
# EM / token-F1 (SQuAD-style, as the benchmark's own metric scripts use)
# ---------------------------------------------------------------------------


def _normalize(text: str) -> str:
    # Dataset gold answers can be JSON scalars (ints for count questions).
    text = str(text).lower()
    text = re.sub(r"\b(a|an|the)\b", " ", text)
    text = re.sub(r"[^a-z0-9 ]", " ", text)
    return " ".join(text.split())


def exact_match(prediction: str, gold: str) -> float:
    return 1.0 if _normalize(prediction) == _normalize(gold) else 0.0


def token_f1(prediction: str, gold: str) -> float:
    prediction_tokens = _normalize(prediction).split()
    gold_tokens = _normalize(gold).split()
    if not prediction_tokens or not gold_tokens:
        return 1.0 if not prediction_tokens and not gold_tokens else 0.0
    common: set[str] = set(prediction_tokens) & set(gold_tokens)
    if not common:
        return 0.0
    precision = len(common) / len(prediction_tokens)
    recall = len(common) / len(gold_tokens)
    return 2 * precision * recall / (precision + recall)


# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------


@dataclass
class InstanceResult:
    question_id: str
    question_type: str
    question: str
    gold_answer: str
    prediction: str = ""
    judged: bool = False
    judge_correct: bool = False
    judge_raw: str = ""
    judge_prompt_kind: str = ""
    em: float = 0.0
    f1: float = 0.0
    ingest_turns: int = 0
    question_turns: int = 0
    cost_usd: dict[str, float] = field(default_factory=lambda: {"ingest": 0.0, "question": 0.0, "judge": 0.0})
    tokens: dict[str, int] = field(default_factory=lambda: {"ingest_in": 0, "ingest_out": 0, "question_in": 0, "question_out": 0})
    settle_found: bool = False
    # "ready" (index applied every write), "timeout", "rebuild_required" (the
    # maintainer paused the scope), or "unchecked" (no principal, or the
    # runtime root is not visible from this machine).
    index_status: str = "unchecked"
    settle_seconds: float = 0.0
    wall_seconds: float = 0.0
    hitl_interruptions: list[dict[str, Any]] = field(default_factory=list)
    error: str = ""
    profiles_seen: list[str] = field(default_factory=list)


def run_instance(
    root_client: Client,
    instance: dict[str, Any],
    args: argparse.Namespace,
    judge_slug: str,
    hitl: HitlAnswerer | None = None,
) -> InstanceResult:
    question_id = instance["question_id"]
    started = time.perf_counter()
    result = InstanceResult(
        question_id=question_id,
        question_type=instance.get("question_type", ""),
        question=instance.get("question", ""),
        gold_answer=instance.get("answer", ""),
    )
    sessions: list[list[dict[str, Any]]] = instance.get("haystack_sessions") or []
    dates: list[str | None] = instance.get("haystack_dates") or []

    slug = f"{WORKSPACE_SLUG_PREFIX}-{question_id}-{secrets.token_hex(2)}"
    create_workspace(root_client, slug, "LongMemEval instance")
    if hitl:
        hitl.set_workspace(slug)
    client = scope_client(root_client, slug)
    current_scope = slug
    try:
        # ---- ingestion -----------------------------------------------------
        if args.ingest == "digest":
            plan: list[tuple[str, list[str]]] = [
                (f"lme-{question_id}-{index:03d}", messages)
                for index, messages in enumerate(digest_session_messages(dates, sessions, args.max_digest_chars))
            ]
        else:
            plan = [
                (f"lme-{question_id}-{index:03d}", messages)
                for index, messages in enumerate(lived_messages(sessions))
            ]
        for ui_thread_id, messages in plan:
            session_id = new_chat_session(client, ui_thread_id)
            for text in messages:
                response = send_message(client, session_id, text, model_selection=args.ingest_model_selection)
                cost, tokens_in, tokens_out = usage_cost(response)
                result.ingest_turns += 1
                result.cost_usd["ingest"] += cost
                result.tokens["ingest_in"] += tokens_in
                result.tokens["ingest_out"] += tokens_out
                profile = usage_profile(response)
                if profile and profile not in result.profiles_seen:
                    result.profiles_seen.append(profile)

        # ---- settle: wait for retrieval to be REAL, not just episodes on
        # disk. Two gates, reported separately: (1) the episode probe (content
        # landed); (2) index_covers_writes — a non-empty index that has applied
        # every journaled write (a manifest alone is NOT enough), with a
        # rebuild-required pause as its own exit. Without it, question-time
        # retrieval silently degrades to lexical head-matching, which is
        # exactly how a fast-lane instance flips correct->wrong while every
        # episode holds the gold sentence.
        # Some dataset instances contain empty haystack sessions; probe with
        # the last turn of the last NON-empty one (the settle substring check
        # needs real content, and sessions[-1][-1] crashes on empties).
        probe_source = next(
            (s[-1].get("content", "") for s in reversed(sessions) if s),
            "conversation transcript",
        )
        probe = distinctive_phrase(probe_source)
        deadline = time.monotonic() + args.settle_timeout
        found = False
        index_ready = False
        while time.monotonic() < deadline:
            try:
                if memory_search(client, probe) > 0:
                    found = True
                    break
            except EvalFailure:
                pass
            time.sleep(SETTLE_POLL_SECONDS)
        principal = getattr(client, "principal", "") or ""
        root = local_runtime_root(client)
        # No principal (fake client, or a session that did not report one) or
        # no visible runtime root means the index cannot be located: say so
        # instead of waiting.
        checkable = bool(principal) and root is not None
        index_paused = False
        index_deadline = time.monotonic() + args.index_wait_timeout
        while checkable and time.monotonic() < index_deadline:
            if index_covers_writes(root, principal, slug):
                index_ready = True
                break
            if index_paused_for_rebuild(root, principal, slug):
                index_paused = True
                break
            time.sleep(SETTLE_POLL_SECONDS)
        if args.settle_extra > 0:
            time.sleep(args.settle_extra)
        # Two separate verdicts: content searchable, and index caught up.
        result.settle_found = found
        result.index_status = (
            "unchecked" if not checkable
            else "ready" if index_ready
            else "rebuild_required" if index_paused
            else "timeout"
        )
        if not principal:
            print(f"  warning: no principal reported, so the memory index for {slug} "
                  "was not checked — retrieval may be fast-snapshot only")
        elif root is None:
            print(f"  warning: runtime root not visible from here (set MAGICIAN_ROOT_DIR), so the "
                  f"memory index for {slug} was not checked — retrieval may be fast-snapshot only")
        elif index_paused:
            print(f"  warning: memory index for {slug} is paused until an explicit rebuild "
                  "(rebuild-required.json) — retrieval is lexical only")
        elif not index_ready:
            print(f"  warning: memory index for {slug} did not apply every write "
                  f"within {args.index_wait_timeout:.0f}s — retrieval may miss the latest sessions")

        # ---- question in a brand-new session (durable memory only) ---------
        question_session = new_chat_session(client, f"lme-{question_id}-question")
        question_text = f"{QUESTION_FRAMING}\n\nQuestion: {instance['question']}"
        response = send_message(client, question_session, question_text, model_selection=args.answer_model_selection)
        result.prediction = assistant_text(response).strip()
        result.question_turns = 1
        cost, tokens_in, tokens_out = usage_cost(response)
        result.cost_usd["question"] = cost
        result.tokens["question_in"] = tokens_in
        result.tokens["question_out"] = tokens_out
        profile = usage_profile(response)
        if profile and profile not in result.profiles_seen:
            result.profiles_seen.append(profile)
    finally:
        # Never purge the workspace a session is scoped to: deleting the
        # engraved workspace makes every later call on that session 400
        # unowned_workspace (found the hard way in the first pilot — one
        # such purge poisoned every remaining instance). Leave the instance
        # scope for the judge workspace first; purging across scopes is the
        # task_recipes purge pattern.
        if not args.keep_workspaces:
            try:
                if current_scope != judge_slug:
                    scope_client(client, judge_slug)
                    current_scope = judge_slug
            except EvalFailure as error:
                print(f"  warning: could not leave {slug} scope; workspace left un-purged: {error}")
            else:
                # The scope dir exists by now (created lazily on first
                # write, so the create-time marker attempt above may have
                # been skipped); a purged-but-on-disk dir is exactly what
                # the offline sweep needs the marker for.
                mark_workspace(client, slug)
                purge_workspace(client, slug)

    # ---- judge: a FRESH judge workspace per instance ----------------------
    # A shared judge workspace would put every earlier judge turn (question,
    # gold answer, verdict) into the judge's own memory during later
    # verdicts. The benchmark's judges are stateless; this keeps ours so too.
    if args.judge and result.prediction:
        judge_ws = f"{WORKSPACE_SLUG_PREFIX}-judge-{question_id}-{secrets.token_hex(2)}"
        create_workspace(client, judge_ws, "LongMemEval judge")
        if hitl:
            hitl.set_workspace(judge_ws)
        judge_client = scope_client(client, judge_ws)
        try:
            prompt = judge_prompt(
                result.question_type, question_id, result.question, result.gold_answer, result.prediction
            )
            kind = "abstention" if "_abs" in question_id else _JUDGE_TYPE_MAP.get(result.question_type, result.question_type)
            result.judge_prompt_kind = kind
            judge_session = new_chat_session(judge_client, f"lme-judge-{question_id}")
            response = send_message(judge_client, judge_session, prompt, model_selection=args.judge_model_selection)
            result.judge_raw = assistant_text(response).strip()
            result.judged = True
            result.judge_correct = judge_label(result.judge_raw)
            cost, _, _ = usage_cost(response)
            result.cost_usd["judge"] = cost
        finally:
            # Back to the run's parking scope first — purging the engraved
            # judge workspace would poison the session.
            try:
                scope_client(client, judge_slug)
                if not args.keep_workspaces:
                    mark_workspace(client, judge_ws)
                    purge_workspace(client, judge_ws)
            except EvalFailure as error:
                print(f"  warning: judge workspace {judge_ws} left un-purged: {error}")

    result.em = exact_match(result.prediction, result.gold_answer)
    result.f1 = token_f1(result.prediction, result.gold_answer)
    result.wall_seconds = time.perf_counter() - started
    if hitl:
        result.hitl_interruptions = hitl.snapshot_and_clear()
    return result


def aggregate(results: list[InstanceResult]) -> dict[str, Any]:
    judged = [r for r in results if r.judged and not r.error]
    checked = [r for r in results if r.index_status != "unchecked"]
    per_type: dict[str, dict[str, Any]] = {}
    for question_type in sorted({r.question_type for r in results}):
        typed = [r for r in judged if r.question_type == question_type]
        per_type[question_type] = {
            "n": len(typed),
            "correct": sum(1 for r in typed if r.judge_correct),
            "accuracy": round(sum(1 for r in typed if r.judge_correct) / len(typed), 4) if typed else None,
        }
    return {
        "n_instances": len(results),
        "n_errors": sum(1 for r in results if r.error),
        "n_judged": len(judged),
        "judge_correct": sum(1 for r in judged if r.judge_correct),
        "judge_accuracy": round(sum(1 for r in judged if r.judge_correct) / len(judged), 4) if judged else None,
        "exact_match": round(sum(r.em for r in judged) / len(judged), 4) if judged else None,
        "token_f1": round(sum(r.f1 for r in judged) / len(judged), 4) if judged else None,
        "per_question_type": per_type,
        "settle_found_rate": round(sum(1 for r in results if r.settle_found) / len(results), 4) if results else None,
        "index_ready_rate": (
            round(sum(1 for r in checked if r.index_status == "ready") / len(checked), 4) if checked else None
        ),
        "cost_usd": {
            "ingest": round(sum(r.cost_usd["ingest"] for r in results), 4),
            "question": round(sum(r.cost_usd["question"] for r in results), 4),
            "judge": round(sum(r.cost_usd["judge"] for r in results), 4),
            "total": round(sum(sum(r.cost_usd.values()) for r in results), 4),
        },
        "ingest_turns_total": sum(r.ingest_turns for r in results),
        "hitl_interruptions_total": sum(len(r.hitl_interruptions) for r in results),
        "avg_instance_wall_seconds": round(sum(r.wall_seconds for r in results) / len(results), 1) if results else None,
    }


def git_commit() -> str:
    try:
        return subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, timeout=10
        ).stdout.strip()
    except Exception:
        return "unknown"


def magician_version(client: Client) -> str:
    try:
        status, raw = client.request("GET", "/health", timeout=10.0)
        return str((json.loads(raw) or {}).get("version", "unknown")) if status == 200 else "unknown"
    except Exception:
        return "unknown"


def dataset_fingerprint(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            hasher.update(block)
    return hasher.hexdigest()[:16]


def write_report(
    output_dir: Path,
    args: argparse.Namespace,
    environment: dict[str, Any],
    results: list[InstanceResult],
    status: str,
) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    summary = aggregate(results)
    report = {
        "kind": "longmemeval",
        "benchmark": "LongMemEval (S variant)",
        "status": status,
        "verdict": "pass" if status == "completed" and summary["n_judged"] > 0 else "fail",
        "environment": environment,
        "protocol": {
            "ingest": args.ingest,
            "settle_timeout_seconds": args.settle_timeout,
            "settle_extra_seconds": args.settle_extra,
            "judge": args.judge,
            "keep_workspaces": args.keep_workspaces,
            "model_selections": {stage: getattr(args, f"{stage}_model_selection") for stage in ("ingest", "answer", "judge")},
        },
        "summary": summary,
        "baselines_reference": BASELINES_S,
        "instances": [r.__dict__ for r in results],
    }
    report_path = output_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2))
    write_html(output_dir / "report.html", report)
    return report_path


def write_html(path: Path, report: dict[str, Any]) -> None:
    summary = report["summary"]
    cost = summary["cost_usd"]
    rows = []
    for instance in report["instances"]:
        rows.append(
            "<tr>"
            f"<td>{escape(instance['question_id'])}</td>"
            f"<td>{escape(instance['question_type'])}</td>"
            f"<td>{'✅' if instance['judge_correct'] else '❌' if instance['judged'] else '—'}</td>"
            f"<td>{escape(instance['prediction'][:120])}</td>"
            f"<td>{escape(instance['gold_answer'][:80])}</td>"
            f"<td>{instance['f1']:.2f}</td>"
            f"<td>${sum(instance['cost_usd'].values()):.3f}</td>"
            f"<td>{'ok' if instance['settle_found'] else 'timeout'} · index {escape(instance.get('index_status', 'unchecked'))}</td>"
            f"<td>{escape(instance['error'][:80])}</td>"
            "</tr>"
        )
    per_type_rows = "".join(
        f"<tr><td>{escape(t)}</td><td>{d['n']}</td><td>{d['correct']}</td><td>{d['accuracy']}</td></tr>"
        for t, d in summary["per_question_type"].items()
    )
    html = f"""<!doctype html><html><head><meta charset="utf-8"><title>LongMemEval — Magician</title>
<style>body{{font-family:ui-sans-serif,system-ui;margin:24px;color:#111}}table{{border-collapse:collapse;margin:12px 0}}
td,th{{border:1px solid #ccc;padding:4px 8px;font-size:13px;text-align:left}}</style></head><body>
<h2>LongMemEval (S) — Magician harness</h2>
<p><b>{escape(report['status'])}</b> · {summary['n_judged']} judged · judge accuracy
<b>{summary['judge_accuracy']}</b> · EM {summary['exact_match']} · F1 {summary['token_f1']}
· cost ${cost['total']} (${cost['ingest']} ingest / ${cost['question']} question / ${cost['judge']} judge)
· {summary['ingest_turns_total']} ingest turns · settle found {summary['settle_found_rate']}</p>
<p>Ingest <b>{escape(report['protocol']['ingest'])}</b> · profiles {escape(', '.join(sorted({p for i in report['instances'] for p in i['profiles_seen']})))}</p>
<h3>Per question type</h3><table><tr><th>type</th><th>n</th><th>correct</th><th>accuracy</th></tr>{per_type_rows}</table>
<h3>Instances</h3><table><tr><th>id</th><th>type</th><th>judge</th><th>prediction</th><th>gold</th><th>F1</th><th>cost</th><th>settle</th><th>error</th></tr>
{''.join(rows)}</table></body></html>"""
    path.write_text(html)


# ---------------------------------------------------------------------------
# Self-test (provider-free)
# ---------------------------------------------------------------------------


class FakeClient(Client):
    """Records sends, returns canned replies. Exercises every code path that
    is not the network itself."""

    def __init__(self, base_url: str = "http://fake", bearer: str | None = "fake"):  # noqa: D107 - no network at all
        super().__init__(base_url, bearer)
        self.sent: list[str] = []
        self.sent_bodies: list[dict[str, Any]] = []

    def json(self, method, path, body=None, query=None, expected=(200,), timeout=120.0):  # noqa: A002
        if path.endswith("/auth/session") or path == "/health":
            return {"workspace": "fake", "version": "test"}
        if path.endswith("/auth/session/scope"):
            return {"token": "rotated"}
        if path.endswith("/chat/new"):
            return {"session": {"id": "sess-" + secrets.token_hex(3)}}
        if "/messages" in path:
            self.sent.append((body or {}).get("text", ""))
            self.sent_bodies.append(dict(body or {}))
            reply = "yes" if "Answer yes or no only." in (body or {}).get("text", "") else "OK"
            return {"assistant_message": {"content": {"type": "text", "text": reply}},
                    "usage": {"costUsd": 0.001, "inputTokens": 100, "outputTokens": 5}}
        if path.endswith("/memory/search"):
            return {"results": [{"episode": "…"}]}
        return {}


def self_test(args: argparse.Namespace) -> int:
    fixture_instance = {
        "question_id": "abc123_abs",
        "question_type": "single-session-user",
        "question": "What is unanswerable here?",
        "answer": "The information is incomplete.",
        "haystack_dates": ["2023/05/01 (Mon)"],
        "haystack_sessions": [[
            {"role": "user", "content": "The river crossing puzzle involves a fox and a chicken and grain."},
            {"role": "assistant", "content": "Classic constraint puzzle; take the chicken first."},
        ] * 200],
    }
    client = FakeClient()
    args.ingest_model_selection = {"model_id": "fixture/luna", "thinking": "none"}
    args.answer_model_selection = {"model_id": "fixture/sol", "thinking": "high"}
    args.judge_model_selection = {"model_id": "fixture/judge", "thinking": "auto"}
    args.judge = True
    args.hitl = False
    args.settle_timeout = 1
    args.settle_extra = 0
    args.keep_workspaces = True
    result = run_instance(client, fixture_instance, args, judge_slug="lmeval-judge-test")
    assert all("profile" not in body and body.get("harness_engine") == "magician" for body in client.sent_bodies)
    assert {body["model_selection"]["thinking"] for body in client.sent_bodies} == {"none", "high", "auto"}
    assert result.judged and result.judge_correct, "fake judge should answer yes"
    assert result.judge_prompt_kind == "abstention", "abstention prompt must be selected for _abs ids"
    assert result.ingest_turns >= 1, "digest ingestion must send at least one message"
    assert result.f1 >= 0.0
    assert result.settle_found, "content settle is reported on its own, not folded into the index check"
    assert result.index_status == "unchecked", "the fake client has no principal or visible root"
    with tempfile.TemporaryDirectory() as tmp:
        index_dir = Path(tmp) / "scopes" / "p" / "w" / "memory" / "index"
        assert not index_covers_writes(Path(tmp), "p", "w"), "no manifest: not ready"
        index_dir.mkdir(parents=True)
        (index_dir / "manifest.json").write_text('{"document_count": 0}')
        assert not index_covers_writes(Path(tmp), "p", "w"), "an empty index is not ready"
        (index_dir / "manifest.json").write_text('{"document_count": 12}')
        (index_dir / "changes.json").write_text('{"entries": {"user": {"generation": 3}}}')
        assert not index_covers_writes(Path(tmp), "p", "w"), "pending journal entries: not ready"
        (index_dir / "changes.json").write_text('{"entries": {}, "next_generation": 4}')
        assert index_covers_writes(Path(tmp), "p", "w")
        assert not index_paused_for_rebuild(Path(tmp), "p", "w")
        (index_dir / "rebuild-required.json").write_text("{}")
        assert index_paused_for_rebuild(Path(tmp), "p", "w")
    assert local_runtime_root(Client("http://10.0.0.5:3002", None)) is None, "a remote root is not visible here"
    assert local_runtime_root(Client("http://127.0.0.1:3002", None)) is not None
    prompt = judge_prompt("temporal-reasoning", "x", "q", "18 days", "19 days")
    assert "off-by-one" in prompt, "temporal template must be the off-by-one one"
    messages = digest_messages(["2023/05/01 (Mon)"], [fixture_instance["haystack_sessions"][0]])
    assert all(len(m) <= MAX_MESSAGE_CHARS for m in messages), "digest chunks must respect the send limit"
    assert len(messages) > 1, "the 80-turn fixture must split into several digest chunks"
    grouped = digest_session_messages(["2023-05-01", "2023-05-02"], [
        fixture_instance["haystack_sessions"][0], [{"role": "user", "content": "Second session only."}],
    ])
    assert len(grouped) == 2 and len(grouped[0]) > 1, "split transcript must preserve its source session"
    assert all("session 1 of 2" in message for message in grouped[0])
    assert "session 2 of 2" in grouped[1][0]
    results = [result]
    report_path = write_report(Path(args.output_dir) / "self-test", args, {"self_test": True}, results, "completed")
    payload = json.loads(report_path.read_text())
    assert payload["verdict"] == "pass" and payload["summary"]["n_judged"] == 1
    print(f"self-test: OK (report at {report_path})")
    return 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base-url", default=os.environ.get("MAGICIAN_API", DEFAULT_BASE_URL))
    parser.add_argument("--bearer", default="", help="bearer token (else MAGICIAN_BEARER_TOKEN, else eval login)")
    parser.add_argument("--dataset", default=str(DEFAULT_DATASET), help="path to the LongMemEval JSON file")
    parser.add_argument("--limit", type=int, default=DEFAULT_LIMIT, help="instances to run (pilot mode)")
    parser.add_argument("--offset", type=int, default=0)
    parser.add_argument("--question-ids", default="", help="comma-separated question_ids to run (overrides limit)")
    parser.add_argument("--ingest", choices=("digest", "lived"), default="digest")
    parser.add_argument(
        "--max-digest-chars",
        type=int,
        default=DEFAULT_DIGEST_CHUNK_CHARS,
        help="digest chunk size; legacy runtimes retain 4,000-character excerpts; source-complete runtimes support the ingress limit",
    )
    add_chat_model_arguments(parser, ("ingest", "answer", "judge"))
    parser.add_argument("--no-judge", dest="judge", action="store_false", help="skip LLM judging (EM/F1 only)")
    parser.add_argument("--no-hitl", dest="hitl", action="store_false", help="disable the automatic user-request answerer")
    parser.add_argument("--settle-timeout", type=float, default=DEFAULT_SETTLE_TIMEOUT_SECONDS)
    parser.add_argument("--settle-extra", type=float, default=DEFAULT_SETTLE_EXTRA_SECONDS)
    parser.add_argument("--index-wait-timeout", type=float, default=600.0,
                        help="how long to wait for the scope's memory index manifest before asking (retrieval is lexical-only until it exists)")
    parser.add_argument("--max-cost-usd", type=float, default=DEFAULT_MAX_COST_USD, help="abort once summed cost exceeds this")
    parser.add_argument("--keep-workspaces", action="store_true", help="do not purge instance workspaces (debugging)")
    parser.add_argument("--output-dir", default=str(DEFAULT_OUTPUT_DIR))
    parser.add_argument("--self-test", action="store_true", help="provider-free plumbing check")
    parser.add_argument("--resume", action="store_true", help="skip instances that already have a record in the output dir")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return self_test(args)

    dataset_path = Path(args.dataset)
    if not dataset_path.is_file():
        print(f"error: dataset not found at {dataset_path} "
              f"(download: https://huggingface.co/datasets/xiaowu0162/longmemeval → longmemeval_s)")
        return 2
    data = json.loads(dataset_path.read_text())
    if args.question_ids:
        wanted = {q.strip() for q in args.question_ids.split(",") if q.strip()}
        instances = [d for d in data if d["question_id"] in wanted]
    else:
        instances = data[args.offset : args.offset + args.limit]
    if not instances:
        print("error: no instances selected")
        return 2
    if args.resume:
        # Server restarts keep killing long serial runs; already-recorded
        # instances are skipped so a relaunch continues where it stopped.
        results_path = Path(args.output_dir) / "results.jsonl"
        done = set()
        if results_path.exists():
            for line in results_path.read_text().splitlines():
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if record.get("judged"):
                    done.add(record.get("question_id"))
        before = len(instances)
        instances = [i for i in instances if i["question_id"] not in done]
        print(f"resume: {before - len(instances)} instance(s) already recorded; {len(instances)} to go")

    client, session = bootstrap_client(args)
    environment = {
        "base_url": args.base_url,
        "principal": session.get("principal"),
        "magician_version": magician_version(client),
        "git_commit": git_commit(),
        "dataset_file": str(dataset_path),
        "dataset_sha256_16": dataset_fingerprint(dataset_path),
        "dataset_instances_total": len(data),
        "instance_selection": args.question_ids or f"offset={args.offset} limit={args.limit}",
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    print(f"LongMemEval: {len(instances)} instances, ingest={args.ingest}, judge={args.judge}")
    print(f"environment: {json.dumps(environment)}")

    try:
        resolve_chat_model_selections(args, ("ingest", "answer", "judge"),
            lambda: client.json("GET", "/api/magician/v2/chat/models", query={"engine": "magician"}))
    except ValueError as error:
        print(f"error: {error}")
        return 2

    judge_slug = f"{WORKSPACE_SLUG_PREFIX}-judge-{secrets.token_hex(3)}"
    create_workspace(client, judge_slug, "LongMemEval judge")

    hitl: HitlAnswerer | None = None
    if args.hitl:
        hitl = HitlAnswerer(lambda: login_client(args))
        hitl.start()

    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    results_path = output_dir / "results.jsonl"
    results: list[InstanceResult] = []
    status = "completed"
    try:
        for index, instance in enumerate(instances, 1):
            print(f"[{index}/{len(instances)}] {instance['question_id']} ({instance.get('question_type')}) …")
            try:
                result = run_instance(client, instance, args, judge_slug, hitl=hitl)
                results.append(result)
                print(
                    f"  judged={'yes' if result.judge_correct else 'no' if result.judged else '—'} "
                    f"f1={result.f1:.2f} settle={'ok' if result.settle_found else 'timeout'} index={result.index_status} "
                    f"cost=${sum(result.cost_usd.values()):.3f} ({result.ingest_turns} ingest turns, "
                    f"{result.wall_seconds:.0f}s)"
                )
            except RuntimeUnavailable as error:
                print(f"  runtime unavailable: {error}")
                status = "aborted"
                break
            except EvalFailure as error:
                results.append(InstanceResult(
                    question_id=instance["question_id"], question_type=instance.get("question_type", ""),
                    question=instance.get("question", ""), gold_answer=instance.get("answer", ""), error=str(error),
                ))
                print(f"  instance failed: {error}")
            with results_path.open("a") as handle:
                handle.write(json.dumps(results[-1].__dict__) + "\n")
            spent = sum(sum(r.cost_usd.values()) for r in results)
            if spent > args.max_cost_usd:
                print(f"cost ceiling ${args.max_cost_usd} exceeded (${spent:.2f}) — stopping")
                status = "aborted"
                break
    finally:
        if hitl:
            hitl.stop()
        if not args.keep_workspaces:
            purge_workspace(client, judge_slug)
        environment["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        # results.jsonl is the source of truth: it carries records appended
        # by earlier processes that ran into this output dir, so a rerun of
        # one instance stitches into the same report instead of clobbering
        # it with a single-instance one. (Strip stale records for an
        # instance before rerunning it — appending adds a second row.)
        stitched: list[InstanceResult] = []
        for line in (results_path.read_text().splitlines() if results_path.exists() else []):
            line = line.strip()
            if not line:
                continue
            try:
                stitched.append(InstanceResult(**json.loads(line)))
            except (json.JSONDecodeError, TypeError) as error:
                print(f"  warning: unreadable prior record skipped: {error}")
        if stitched:
            results = stitched
        report_path = write_report(output_dir, args, environment, results, status)
        print(f"report: {report_path}")
        print(f"summary: {json.dumps(aggregate(results))}")
    return 0 if status == "completed" and any(r.judged for r in results) else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeUnavailable as error:
        print(f"ABORTED (runtime unavailable): {error}", file=sys.stderr)
        sys.exit(2)
    except EvalFailure as error:
        print(f"FAILED: {error}", file=sys.stderr)
        sys.exit(2)

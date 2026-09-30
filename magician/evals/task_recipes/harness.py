#!/usr/bin/env python3
"""Task Recipes live eval — the harness the p1..pN cases run on.

This is the Python port of the former Rust example
(`magician/examples/task_recipes_fixture_eval`). It exists because a case is
HTTP + JSON + file reads and nothing more, so editing or adding one should cost
a file save, not a 40-minute crate rebuild. Only the compiler-internal unit
tests stay in Rust; everything here talks to the live service.

Contract, verified against the running stack:
  auth    POST {V2}/auth/login {username,password} -> {token}
          GET/POST {V2}/workspaces  (ensure the disposable eval scope exists)
          POST {V2}/auth/session/scope {workspace} -> {token} (workspace-bound)
          GET  {V2}/auth/session -> {principal, workspace}
  seal    magician.bin seal-stateless-loop-cutover --principal --workspace
          --deployment-id task-recipes-fixture-eval --confirm-legacy-writers-drained
  run     POST {V3}/tasks {title,description,agent_id,ui_thread_id,created_by,approved}
          POST {V3}/tasks/{id}/execute {}
          GET  {V3}/tasks/{id}/executions   (poll status)
          GET  {V3}/tasks/{id}              (details / outcome)
  mining  GET  {V2}/api-mining/recipes ; GET {V2}/api-mining/recipes/{id}
          POST {V2}/api-mining/origins/{key}/allow-replay {origin_url,allow_replay}
  hitl    GET  {V2}/user-requests  (filter api_replay_approval)
          POST {V2}/hitl/{id}/respond {source,value,channel}
  evidence  <root>/scopes/<p>/<w>/tasks/<t>/executions/<e>/events.jsonl
            <root>/scopes/<p>/<w>/api_mining/<e>/trace_*.jsonl
"""

from __future__ import annotations

import json
import os
import subprocess
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

V2 = "/api/magician/v2"
V3 = "/api/magician/v3"
REPLAY_APPROVAL_REQUEST_TYPE = "api_replay_approval"


# --------------------------------------------------------------------------- #
# Result shapes                                                               #
# --------------------------------------------------------------------------- #
@dataclass
class Gate:
    id: str
    passed: bool
    detail: str = ""


@dataclass
class Expect:
    """Empty answer accepts any answer. `browserless` asserts the trio that
    proves no browser ran: signals unchanged, no capture trace, replay events."""
    answer: list[str] = field(default_factory=list)
    outcome_type: str | None = None
    not_outcome_type: str | None = None
    browserless: bool = False


@dataclass
class Phase:
    id: str
    task_id: str | None = None
    execution_id: str | None = None
    status: str = ""
    outcome_type: str = ""
    summary_excerpt: str = ""
    gates: list[Gate] = field(default_factory=list)
    error: str | None = None

    def passed(self) -> bool:
        return self.error is None and bool(self.gates) and all(g.passed for g in self.gates)


@dataclass
class Case:
    id: str
    site: str
    origin: str
    phases: list[Phase] = field(default_factory=list)
    recipe_id: str | None = None
    error: str | None = None

    def finish(self) -> "Case":
        self.passed = self.error is None and bool(self.phases) and all(p.passed() for p in self.phases)
        return self

    passed: bool = False


@dataclass
class Observed:
    task_id: str
    execution_id: str
    status: str
    outcome_type: str
    summary: str
    events: list[dict]
    tabs_before: int
    tabs_after: int
    seq_before: int
    seq_after: int
    capture_traces: int


# --------------------------------------------------------------------------- #
# HTTP + auth                                                                 #
# --------------------------------------------------------------------------- #
class HttpError(RuntimeError):
    def __init__(self, method: str, url: str, status: int, body: str):
        super().__init__(f"{method} {url} -> {status}: {body[:200]}")
        self.status = status


class Magician:
    def __init__(self, base: str, magicutor_base: str, runtime_root: Path):
        self.base = base.rstrip("/")
        self.magicutor_base = magicutor_base.rstrip("/")
        self.runtime_root = runtime_root
        self.bearer = ""
        self.principal = ""
        self.workspace = ""

    # -- low level -------------------------------------------------------- #
    def _request(self, method: str, path: str, body: Any | None = None,
                 bearer: str | None = None, timeout: int = 120) -> Any:
        url = f"{self.base}{path}" if path.startswith("/") else path
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        token = bearer if bearer is not None else self.bearer
        if token:
            req.add_header("Authorization", f"Bearer {token}")
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                raw = resp.read().decode()
        except urllib.error.HTTPError as exc:
            raise HttpError(method, url, exc.code, exc.read().decode(errors="replace")) from None
        return json.loads(raw) if raw.strip() else {}

    def get(self, path: str, **kw) -> Any:
        return self._request("GET", path, **kw)

    def post(self, path: str, body: Any | None = None, **kw) -> Any:
        return self._request("POST", path, body if body is not None else {}, **kw)

    # -- workspace lifecycle ---------------------------------------------- #
    def list_workspaces(self) -> list[str]:
        listed = self.get(f"{V2}/workspaces")
        rows = listed if isinstance(listed, list) else listed.get("workspaces", [])
        return [w.get("id", "") for w in rows if w.get("id")]

    def purge_workspace(self, slug: str) -> None:
        """Delete a workspace and its data: the registry row goes now, the
        directory at the server's next start (`?purge=true`), so this is safe
        while the service runs — nothing is removed from under it."""
        self._request("DELETE", f"{V2}/workspaces/{slug}?purge=true")

    # -- connect / seal --------------------------------------------------- #
    def connect(self, workspace_slug: str) -> None:
        bearer = _env("MAGICIAN_BEARER_TOKEN")
        if not bearer:
            user = _eval_credential("MAGICIAN_EVAL_USERNAME", self.runtime_root)
            pw = _eval_credential("MAGICIAN_EVAL_PASSWORD", self.runtime_root)
            if not (user and pw):
                raise RuntimeError(
                    "no credentials: set MAGICIAN_BEARER_TOKEN, or MAGICIAN_EVAL_USERNAME + "
                    "MAGICIAN_EVAL_PASSWORD in the env or $MAGICIAN_ROOT_DIR/.env.development"
                )
            bearer = self._login_and_scope(user, pw, workspace_slug)
        self.bearer = bearer
        session = self.get(f"{V2}/auth/session")
        self.principal = _first_str(session, "principal") or ""
        self.workspace = _first_str(session, "workspace") or ""
        if self.workspace != workspace_slug:
            raise RuntimeError(f"bearer bound to workspace `{self.workspace}`, expected `{workspace_slug}`")

    def _login_and_scope(self, user: str, pw: str, slug: str) -> str:
        login = self.post(f"{V2}/auth/login", {"username": user, "password": pw})
        session_token = _first_str(login, "token")
        if not session_token:
            raise RuntimeError("login omitted token")
        workspaces = self.get(f"{V2}/workspaces", bearer=session_token)
        rows = workspaces if isinstance(workspaces, list) else workspaces.get("workspaces", [])
        if not any(w.get("id") == slug for w in rows):
            self.post(f"{V2}/workspaces",
                      {"slug": slug, "display_name": "Task Recipes eval",
                       "description": "Disposable scope for the Task Recipes live eval"},
                      bearer=session_token)
        rotated = self.post(f"{V2}/auth/session/scope", {"workspace": slug}, bearer=session_token)
        token = _first_str(rotated, "token")
        if not token:
            raise RuntimeError("rotated session omitted token")
        return token

    def seal(self, magician_bin: Path) -> bool:
        """Seal the stateless-loop cutover; the stateless driver refuses an
        unsealed scope. Returns True if it was already sealed."""
        binpath = magician_bin.resolve()
        proc = subprocess.run(
            [str(binpath), "seal-stateless-loop-cutover",
             "--principal", self.principal, "--workspace", self.workspace,
             "--deployment-id", "task-recipes-fixture-eval",
             "--confirm-legacy-writers-drained"],
            capture_output=True, text=True)
        if proc.returncode != 0 and "already retired by deployment" in proc.stderr:
            return True
        sealed = None
        for line in reversed(proc.stdout.splitlines()):
            try:
                row = json.loads(line.strip())
            except json.JSONDecodeError:
                continue
            if row.get("legacy_writers_retired") is True:
                sealed = row
                break
        if sealed is not None and proc.returncode == 0:
            return bool(sealed.get("already_sealed", False))
        last = (proc.stderr.strip().splitlines() or [""])[-1]
        raise RuntimeError(f"seal did not publish for {self.principal}/{self.workspace} "
                           f"(exit {proc.returncode}): {last}")

    # -- tasks ------------------------------------------------------------ #
    def create_and_execute(self, title: str, description: str, ui_thread: str) -> tuple[str, str]:
        created = self.post(f"{V3}/tasks", {
            "title": title, "description": description, "agent_id": "personal-assistant",
            "ui_thread_id": ui_thread, "created_by": "user", "approved": True})
        task_id = _first_str(created, "task_id")
        if not task_id:
            raise RuntimeError("create response omitted task_id")
        accepted = self.post(f"{V3}/tasks/{task_id}/execute", {})
        execution_id = _first_str(accepted, "execution_id")
        if not execution_id:
            raise RuntimeError("execute response omitted execution_id")
        return task_id, execution_id

    def wait_terminal(self, task_id: str, execution_id: str, timeout: int,
                      on_pending: Callable[[list[dict]], tuple[str, str] | None] | None = None
                      ) -> tuple[str, str, str]:
        """Poll to a terminal state. Auto-answers replay-write approvals via
        on_pending. Returns (status, outcome_type, summary) from task details;
        the canonical outcome is read from events afterwards."""
        started = time.time()
        while True:
            if on_pending:
                pending = self._pending_replay_approvals()
                answer = on_pending(pending)
                if answer:
                    request_id, option = answer
                    self._respond_approval(request_id, option)
            executions = self.get(f"{V3}/tasks/{task_id}/executions")
            status = _execution_status(executions, execution_id)
            if status in ("completed", "failed", "cancelled", "canceled"):
                details = self.get(f"{V3}/tasks/{task_id}")
                otype, summary = _outcome_for_execution(details, execution_id)
                return status, otype, summary
            if time.time() - started >= timeout:
                raise TimeoutError(f"task {task_id} did not reach a terminal state within {timeout}s")
            time.sleep(2)

    def _pending_replay_approvals(self) -> list[dict]:
        try:
            listing = self.get(f"{V2}/user-requests")
        except HttpError:
            return []
        reqs = listing.get("requests", []) if isinstance(listing, dict) else []
        return [r for r in reqs if r.get("request_type") == REPLAY_APPROVAL_REQUEST_TYPE]

    def _respond_approval(self, request_id: str, option: str) -> None:
        self.post(f"{V2}/hitl/{request_id}/respond",
                  {"source": "user_request", "value": {"type": "choice", "selected_id": option},
                   "channel": "web"})

    # -- mining ----------------------------------------------------------- #
    def wait_for_recipe_bound(self, task_id: str, timeout: int) -> tuple[str, dict] | None:
        started = time.time()
        while True:
            listing = self.get(f"{V2}/api-mining/recipes")
            rows = listing if isinstance(listing, list) else listing.get("recipes", [])
            for row in rows:
                rid = row.get("id")
                if not rid:
                    continue
                detail = self.get(f"{V2}/api-mining/recipes/{rid}")
                bound = _first_str(detail, "task_id") == task_id or (
                    task_id in (detail.get("task_ids") or []))
                if bound:
                    return rid, detail
            if time.time() - started >= timeout:
                return None
            time.sleep(1)

    def allow_origin_replay(self, origin_url: str) -> Any:
        key = origin_url.replace("://", "___")
        for ch in ".", "/", ":":
            key = key.replace(ch, "_")
        return self.post(f"{V2}/api-mining/origins/{key}/allow-replay",
                         {"origin_url": origin_url, "allow_replay": True})

    # -- signals / evidence ---------------------------------------------- #
    def browser_signals(self) -> tuple[int, int]:
        """(magicutor_tabs, browser_only_sequences) — unchanged across a
        browserless replay is the core no-browser proof."""
        try:
            targets = self.get(f"{self.magicutor_base}/json/list", timeout=8)
        except Exception:
            return 0, 0
        pages = [t for t in targets if isinstance(t, dict) and t.get("type") == "page"]
        return len(pages), len(pages)

    def scope_dir(self) -> Path:
        return self.runtime_root / "scopes" / self.principal / self.workspace

    def execution_events(self, task_id: str, execution_id: str) -> list[dict]:
        path = (self.scope_dir() / "tasks" / task_id / "executions" / execution_id / "events.jsonl")
        if not path.exists():
            return []
        out = []
        for line in path.read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                continue
        return out

    def capture_trace_count(self, execution_id: str) -> int:
        d = self.scope_dir() / "api_mining" / execution_id
        if not d.is_dir():
            return 0
        return sum(1 for f in d.iterdir()
                   if f.name.startswith("trace_") and f.name.endswith(".jsonl"))


# --------------------------------------------------------------------------- #
# Evidence helpers                                                            #
# --------------------------------------------------------------------------- #
def terminal_outcome(events: list[dict]) -> tuple[str, str, str] | None:
    for event in reversed(events):
        if _event_type(event) != "execution.outcome_observed":
            continue
        payload = event.get("payload", event)
        if payload.get("execution_status") in ("completed", "failed", "cancelled", "canceled"):
            return (payload.get("execution_status", ""), payload.get("outcome_type", ""),
                    payload.get("summary", ""))
    return None


def has_event(events: list[dict], wanted: str) -> bool:
    for event in events:
        if _event_type(event) == wanted:
            return True
        payload = event.get("payload", {})
        if isinstance(payload, dict) and payload.get("kind") == wanted:
            return True
    return False


def _event_type(event: dict) -> str:
    return event.get("event_type") or event.get("type") or ""


# --------------------------------------------------------------------------- #
# Gate engine                                                                 #
# --------------------------------------------------------------------------- #
def gates_for(obs: Observed, expect: Expect) -> list[Gate]:
    gates = [Gate("task_completed", obs.status == "completed", f"status={obs.status}")]
    if expect.answer:
        missing = [n for n in expect.answer if not answer_matches(obs.summary, n)]
        gates.append(Gate("answer_matches_live_api", not missing,
                          f"summary carries {expect.answer!r}" if not missing
                          else f"missing {missing!r} in summary: {obs.summary}"))
    if expect.outcome_type is not None:
        gates.append(Gate("outcome_type", obs.outcome_type == expect.outcome_type,
                          f"outcome_type={obs.outcome_type} (expected {expect.outcome_type})"))
    if expect.not_outcome_type is not None:
        gates.append(Gate("outcome_not_replay", obs.outcome_type != expect.not_outcome_type,
                          f"outcome_type={obs.outcome_type} (must not be {expect.not_outcome_type})"))
    if expect.browserless:
        gates.append(Gate("browser_signals_unchanged",
                          obs.tabs_before == obs.tabs_after and obs.seq_before == obs.seq_after,
                          f"tabs {obs.tabs_before}->{obs.tabs_after} seq {obs.seq_before}->{obs.seq_after}"))
        gates.append(Gate("no_capture_trace", obs.capture_traces == 0,
                          f"{obs.capture_traces} capture trace file(s)"))
        started = has_event(obs.events, "recipe.replay.started")
        completed = has_event(obs.events, "recipe.replay.completed")
        gates.append(Gate("timeline_recipe_replay", started and completed,
                          f"started={started} completed={completed}"))
    return gates


def answer_matches(summary: str, needle: str) -> bool:
    """Substring match with digit-group separators folded, then a numeric
    fallback so `1284.5` matches `1284.50`. Mirrors the Rust driver."""
    def norm(text: str) -> str:
        chars = list(text.lower())
        out = []
        for i, ch in enumerate(chars):
            between = 0 < i < len(chars) - 1 and chars[i - 1].isdigit() and chars[i + 1].isdigit()
            if ch in (",", " ", " ") and between:
                continue
            out.append(ch)
        return "".join(out)
    s, n = norm(summary), norm(needle)
    if n in s:
        return True
    try:
        wanted = float(n)
    except ValueError:
        return False
    tok = ""
    for ch in s + " ":
        if ch.isdigit() or ch == ".":
            tok += ch
        else:
            if tok:
                try:
                    if abs(float(tok.strip(".")) - wanted) < 1e-9 * max(1.0, abs(wanted)):
                        return True
                except ValueError:
                    pass
                tok = ""
    return False


# --------------------------------------------------------------------------- #
# small utilities                                                             #
# --------------------------------------------------------------------------- #
def _env(key: str) -> str:
    return (os.environ.get(key) or "").strip()


def _eval_credential(key: str, runtime_root: Path) -> str | None:
    if v := _env(key):
        return v
    for name in (".env.development", ".env"):
        path = runtime_root / name
        if not path.exists():
            continue
        for line in path.read_text().splitlines():
            line = line.strip()
            line = line[len("export "):] if line.startswith("export ") else line
            if "=" not in line:
                continue
            k, _, v = line.partition("=")
            if k.strip() == key:
                v = v.strip().strip('"').strip("'")
                if v:
                    return v
    return None


def _first_str(value: Any, key: str) -> str | None:
    if isinstance(value, dict):
        if isinstance(value.get(key), str):
            return value[key]
        for v in value.values():
            found = _first_str(v, key)
            if found:
                return found
    return None


def _execution_status(executions: Any, execution_id: str) -> str | None:
    rows = executions if isinstance(executions, list) else (
        executions.get("executions") if isinstance(executions, dict) else None)
    for row in rows or []:
        if row.get("execution_id") == execution_id or row.get("id") == execution_id:
            return (row.get("status") or "").lower()
    return None


def _outcome_for_execution(details: Any, execution_id: str) -> tuple[str, str]:
    # Best-effort; the canonical outcome comes from events afterwards.
    for row in _iter_executions(details):
        if row.get("execution_id") == execution_id or row.get("id") == execution_id:
            return (row.get("outcome_type") or "", row.get("outcome_summary") or row.get("summary") or "")
    return "", ""


def _iter_executions(details: Any):
    if isinstance(details, dict):
        for key in ("executions", "task"):
            sub = details.get(key)
            if isinstance(sub, list):
                yield from (r for r in sub if isinstance(r, dict))
            elif isinstance(sub, dict):
                yield from _iter_executions(sub)

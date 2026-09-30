"""An external harness on the plane MCP door — the conformance rig's plane lane.

Invoked by eval-harness-conformance-live.py --lane plane. Two case kinds:

* `scripted_door` — a stdlib MCP client (no SDK) mints a `plt_` terminal
  grant with the session bearer, opens a door session that offers form
  elicitation, lists the catalog, creates and runs a nonce task through the
  door, and answers the run's one question from a fixture word inside
  `wait_for_run`; around the real answer it proves the door refuses another
  session's answer and a replay, and after the run that a revoked grant
  refuses every call. The effect is the answered word in the task's file and
  reply.
* `cli_door` — each installed CLI (claude, codex, grok, agy) is pointed at
  the door exactly the way its engine points it, holding nothing but the
  `plt_` grant (admitted only by the door), and asked to create the nonce
  task. The effect is the task from Magician's side; the CLI's own tool
  trace is the proof of method.

Tasks and fixtures are kept for inspection; grants are revoked.
"""
from __future__ import annotations

import html
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

TERMINAL = {"completed", "failed", "cancelled", "canceled"}
SCRIPTED = "scripted"
CLI_ENGINES = ("claude_code", "codex", "grok", "agy")
MATRIX = ((SCRIPTED, "scripted_door"),) + tuple((engine, "cli_door") for engine in CLI_ENGINES)
DOOR_PATH = "/api/magician/v2/plane/mcp"
PROTOCOL_VERSION = "2025-11-25"
RUN_TOOLS = ("create_task", "run_task", "wait_for_run")
GRANT_ENV_VAR = "MAGICIAN_PLANE_GRANT"
SERVER_NAME = "magician_plane"
# Every native tool grok would otherwise reach for; mirrors the engine.
GROK_NATIVE_DENYLIST = ("run_terminal_cmd,run_terminal_command,grep,read_file,search_replace,list_dir,web_search,"
                        "web_fetch,todo_write,task,Agent,spawn_subagent,memory_search,image_gen,image_edit,"
                        "image_to_video,edit_file,write_file,glob,bash,get_command_or_subagent_output,"
                        "wait_commands_or_subagents")


def login(args, client):
    """Same rule as the execution lane: a bearer from the environment, else a
    session login from MAGICIAN_EVAL_USERNAME / MAGICIAN_EVAL_PASSWORD."""
    if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip():
        return
    username = os.environ.get("MAGICIAN_EVAL_USERNAME", "").strip()
    password = os.environ.get("MAGICIAN_EVAL_PASSWORD", "")
    if not username:
        raise RuntimeError("set MAGICIAN_BEARER_TOKEN or MAGICIAN_EVAL_USERNAME/MAGICIAN_EVAL_PASSWORD")
    status, raw, _ = client.request("POST", "/api/magician/v2/auth/login",
                                    body={"username": username, "password": password})
    if status not in (200, 201):
        raise RuntimeError(f"login refused with HTTP {status}")
    os.environ["MAGICIAN_BEARER_TOKEN"] = json.loads(raw)["token"]


# ---------------------------------------------------------------------------
# The scripted door client
# ---------------------------------------------------------------------------


class SseReader:
    """Frames off a `text/event-stream` body as they arrive; None at the end."""

    def __init__(self, body):
        self.body = body

    def next(self):
        data = []
        while True:
            line = self.body.readline()
            if not line:
                return json.loads("".join(data)) if data else None
            text = line.decode("utf-8", "replace").rstrip("\r\n")
            if text == "":
                if data:
                    return json.loads("".join(data))
                continue
            if text.startswith("data:"):
                data.append(text[5:].strip())


def elicitation_answer(prompt, word):
    """Fill the door's requested form from its schema: the fixture word for
    every text field, the first option for choices, `true` for a
    confirmation; optional fields stay unanswered."""
    schema = prompt.get("params", {}).get("requestedSchema", {})
    properties = schema.get("properties", {})
    required = set(schema.get("required", []))
    content = {}
    for name, field in properties.items():
        if name not in required:
            continue
        kind = field.get("type")
        if kind == "boolean":
            content[name] = True
        elif kind == "array":
            options = field.get("items", {}).get("anyOf", [])
            content[name] = [options[0]["const"]] if options else []
        elif "oneOf" in field:
            content[name] = field["oneOf"][0]["const"]
        else:
            content[name] = word
    return {"action": "accept", "content": content}


class DoorSession:
    """One MCP session on the plane door under a terminal grant."""

    def __init__(self, client, token, runtime_unavailable, name="plane-conformance"):
        self.client, self.token, self.runtime_unavailable, self.name = client, token, runtime_unavailable, name
        self.session = None
        self.rpc_id = 0

    def _request(self, body, accept):
        headers = {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json",
                   "Accept": accept, "MCP-Protocol-Version": PROTOCOL_VERSION}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        return Request(self.client._url(DOOR_PATH, None), data=json.dumps(body).encode(), headers=headers)

    def initialize(self, capabilities):
        result = self.rpc("initialize", {"protocolVersion": PROTOCOL_VERSION, "capabilities": capabilities,
                                         "clientInfo": {"name": self.name, "version": "1"}})
        return result

    def rpc(self, method, params, timeout=60):
        """A call whose whole answer fits one response: JSON, or an SSE body
        read to its end. Returns the result, raising on a refusal."""
        self.rpc_id += 1
        request = self._request({"jsonrpc": "2.0", "id": self.rpc_id, "method": method, "params": params},
                                "application/json, text/event-stream")
        try:
            with urlopen(request, timeout=timeout) as response:
                self.session = response.headers.get("Mcp-Session-Id", self.session)
                raw = response.read(4 * 1024 * 1024 + 1)
        except HTTPError:
            raise
        except (URLError, TimeoutError, OSError) as error:
            raise self.runtime_unavailable(f"door {method}: {error}") from error
        if len(raw) > 4 * 1024 * 1024:
            raise RuntimeError("door response exceeded the evaluation bound")
        try:
            payload = json.loads(raw)
        except ValueError:
            frames = [json.loads(line[5:].strip()) for line in raw.decode().splitlines() if line.startswith("data:")]
            payload = next(row for row in frames if row.get("id") == self.rpc_id)
        if payload.get("error") or payload.get("result", {}).get("isError"):
            raise RuntimeError(f"door {method} refused: {json.dumps(payload)[:500]}")
        return payload.get("result", {})

    def call_streaming(self, name, arguments, timeout):
        """Open a tools/call whose stream may carry questions before its result.
        Returns (request id, reader); the caller answers prompts as they come."""
        self.rpc_id += 1
        request = self._request({"jsonrpc": "2.0", "id": self.rpc_id, "method": "tools/call",
                                 "params": {"name": name, "arguments": arguments}}, "text/event-stream")
        try:
            response = urlopen(request, timeout=timeout)
        except HTTPError:
            raise
        except (URLError, TimeoutError, OSError) as error:
            raise self.runtime_unavailable(f"door tools/call {name}: {error}") from error
        self.session = response.headers.get("Mcp-Session-Id", self.session)
        return self.rpc_id, SseReader(response)

    def answer(self, prompt_id, result):
        """Post a JSON-RPC result for a prompt; the door's status is the verdict
        (202 accepted, 400 unknown_elicitation_response)."""
        request = self._request({"jsonrpc": "2.0", "id": prompt_id, "result": result}, "application/json")
        try:
            with urlopen(request, timeout=30) as response:
                return response.status
        except HTTPError as error:
            return error.code
        except (URLError, TimeoutError, OSError) as error:
            raise self.runtime_unavailable(f"door answer: {error}") from error

    def status_of(self, method, params):
        """The HTTP status a call gets, refusals included — for the revoked-grant probe."""
        self.rpc_id += 1
        request = self._request({"jsonrpc": "2.0", "id": self.rpc_id, "method": method, "params": params},
                                "application/json, text/event-stream")
        try:
            with urlopen(request, timeout=30) as response:
                return response.status
        except HTTPError as error:
            return error.code
        except (URLError, TimeoutError, OSError) as error:
            raise self.runtime_unavailable(f"door {method}: {error}") from error


def mint_grant(client, label, workspace, engine, timeout_secs, allowed_tools=()):
    payload, _ = client.json("POST", "/api/magician/v2/plane/grants", body={
        "label": label, "workspace": workspace, "agent_identity": "personal-assistant",
        "harness_engine": engine, "ttl_hours": 1, "max_usd": 2.0,
        "max_wall_clock_secs": int(timeout_secs), "max_concurrent_runs": 1,
        "allowed_tools": list(allowed_tools),
    }, expected=(201,))
    return payload["grant"]["id"], payload["token"]


def revoke_grant(client, grant_id):
    client.json("DELETE", f"/api/magician/v2/plane/grants/{quote(grant_id)}", expected=(200, 204))


def scripted_prompt(word_hint, destination):
    return (
        "Before doing anything else you MUST ask me exactly one question using your user-input tool "
        f"({word_hint}): \"Which word should the result contain?\" Wait for my answer. "
        f"Then write the file {destination} containing exactly the word I answered and nothing else, "
        "and reply with that word. Do not guess the word; it only exists in my answer."
    )


def grade_scripted(evidence):
    word = evidence.get("word", "")
    answer = evidence.get("answer") or ""
    return {
        "session_established": bool(evidence.get("session_established")),
        "catalog_advertises_run_tools": all(tool in (evidence.get("catalog") or []) for tool in RUN_TOOLS),
        "task_created_via_door": bool(evidence.get("task_id")),
        "run_launched": bool(evidence.get("execution_id")),
        "elicitation_observed": bool(evidence.get("prompts")),
        "answer_accepted": evidence.get("answer_status") == 202,
        "cross_session_refused": evidence.get("cross_session_status") not in (None, 202),
        "replay_refused": evidence.get("replay_status") not in (None, 202),
        "run_completed": evidence.get("status") == "completed",
        "effect": bool(word) and (evidence.get("written") or "").strip() == word and word in answer,
        "revoke_refuses_calls": evidence.get("revoked_call_status") in (401, 403),
    }


def run_scripted_case(args, client, runtime_unavailable):
    nonce = secrets.token_hex(6)
    word = args.plane_answer_word or f"amber-{secrets.token_hex(4)}"
    root = Path(tempfile.mkdtemp(prefix=f"hc-plane-{nonce}-", dir="/tmp"))
    destination = root / "result.txt"
    evidence = {"engine": SCRIPTED, "case": "scripted_door", "repeat": 1, "nonce": nonce, "word": word,
                "fixture_dir": str(root), "verdict": "fail", "gates": {}, "cleanup_errors": [], "prompts": []}
    grant_id = None
    started_ms, started = int(time.time() * 1000), time.monotonic()
    door = other = None
    try:
        identity, _ = client.json("GET", "/api/magician/v2/auth/session")
        grant_id, token = mint_grant(client, f"hc-plane-{nonce}", identity["workspace"], "magician",
                                     args.turn_timeout_secs)
        evidence["grant_id"] = grant_id
        door = DoorSession(client, token, runtime_unavailable)
        door.initialize({"elicitation": {"form": {}}})
        evidence["session_established"] = bool(door.session)
        tools = door.rpc("tools/list", {})
        evidence["catalog"] = [tool.get("name") for tool in tools.get("tools", [])]
        # `run: "manual"` leaves the task ready: the door's create_task would
        # otherwise dispatch it itself, and only run_task registers a run
        # origin that wait_for_run will accept from this session.
        created = door.rpc("tools/call", {"name": "create_task", "arguments": {
            "title": f"HC-{nonce}", "description": scripted_prompt("ask, do not guess", destination),
            "agent_id": "personal-assistant", "run": "manual"}})
        evidence["create_task_result"] = created
        task_id = extract_id(created, "task_id")
        evidence["task_id"] = task_id
        launched = door.rpc("tools/call", {"name": "run_task", "arguments": {"task_id": task_id}})
        execution_id = extract_id(launched, "execution_id")
        evidence["execution_id"] = execution_id
        print(f"  {SCRIPTED}/scripted_door started {execution_id}", flush=True)
        # A second session on the same grant: its answer to our prompt must be refused.
        other = DoorSession(client, token, runtime_unavailable, name="plane-conformance-other")
        other.initialize({"elicitation": {"form": {}}})
        deadline = time.monotonic() + args.turn_timeout_secs
        status = "unknown"
        while time.monotonic() < deadline:
            request_id, reader = door.call_streaming("wait_for_run", {"execution_id": execution_id, "timeout_secs": 120},
                                                     timeout=150)
            while True:
                frame = reader.next()
                if frame is None:
                    break
                if frame.get("method") == "elicitation/create":
                    evidence["prompts"].append(frame)
                    reply = elicitation_answer(frame, word)
                    if "cross_session_status" not in evidence:
                        evidence["cross_session_status"] = other.answer(frame["id"], reply)
                    evidence["answer_status"] = door.answer(frame["id"], reply)
                    if "replay_status" not in evidence:
                        evidence["replay_status"] = door.answer(frame["id"], reply)
                    continue
                if frame.get("id") == request_id:
                    evidence.setdefault("wait_results", []).append(frame)
                    text = json.dumps(frame)
                    if frame.get("error") or frame.get("result", {}).get("isError"):
                        raise RuntimeError(f"wait_for_run refused: {text[:400]}")
                    break
            payload, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}/executions/{quote(execution_id)}")
            status = payload.get("state", {}).get("status", "unknown")
            task, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}")
            task_state = task["task"]["state"]
            if status in TERMINAL:
                if not task_state.get("synthesis_pending_executions"):
                    break
                # The run is done and its answer is being published; nothing
                # more comes through the door, so wait on the task instead.
                time.sleep(2)
        evidence["status"] = status
        evidence["answer"] = read_output(client, task_id, task_state.get("primary_user_output_id"))
        try:
            evidence["written"] = destination.read_text()
        except OSError:
            evidence["written"] = None
        evidence["events"] = events_for(client, task_id, execution_id, started_ms)
        revoke_grant(client, grant_id)
        grant_id = None
        evidence["revoked_call_status"] = door.status_of("tools/list", {})
        evidence["gates"] = grade_scripted(evidence)
        evidence["verdict"] = "pass" if all(evidence["gates"].values()) else "fail"
    except runtime_unavailable as error:
        evidence["verdict"] = "inconclusive"
        evidence["error"] = f"Runtime unavailable during evaluation: {error}"
    except Exception as error:
        evidence["error"] = f"{type(error).__name__}: {error}"
        evidence["gates"] = grade_scripted(evidence)
    finally:
        if grant_id:
            try:
                revoke_grant(client, grant_id)
            except Exception as error:
                evidence["cleanup_errors"].append(f"revoke: {error}")
        evidence["latency_ms"] = round((time.monotonic() - started) * 1000)
        retained = args.output_dir / "cases" / nonce
        retained.mkdir(parents=True, exist_ok=True)
        (retained / "evidence.json").write_text(json.dumps(evidence, indent=2, default=str))
    print(f"  {SCRIPTED}/scripted_door: {evidence['verdict']} {evidence['gates']} {evidence.get('error', '')}", flush=True)
    return evidence


def extract_id(result, key):
    """An id from a tool result: the result's own field (run_task), then
    structured content, then the text (create_task's JSON)."""
    if isinstance(result.get(key), str) and result[key]:
        return result[key]
    structured = result.get("structuredContent") or {}
    if isinstance(structured, dict) and structured.get(key):
        return structured[key]
    for item in result.get("content", []):
        text = item.get("text", "") if isinstance(item, dict) else ""
        try:
            parsed = json.loads(text)
            if isinstance(parsed, dict) and parsed.get(key):
                return parsed[key]
        except ValueError:
            pass
        match = re.search(rf"{key}[\"']?\s*[:=]\s*[\"']?([A-Za-z0-9_\-]+)", text)
        if match:
            return match.group(1)
    raise RuntimeError(f"tool result carried no {key}: {json.dumps(result)[:300]}")


def read_output(client, task_id, output_id, execution_id=None):
    """A task output is read by the relative path its outputs index names, not
    by its id; the execution lane's reader knows that."""
    from eval_harness_execution import read_output as read_indexed_output
    return read_indexed_output(client, task_id, output_id, execution_id)


def events_for(client, task_id, execution_id, started_ms):
    status, raw, _ = client.request("GET", "/api/magician/v3/events", query={
        "task_id": task_id, "execution_id": execution_id, "since": started_ms, "limit": 4000, "backfill_only": "true"})
    if status != 200:
        return []
    return [json.loads(line) for line in raw.decode().splitlines() if line.strip()]


# ---------------------------------------------------------------------------
# Real CLIs on the door
# ---------------------------------------------------------------------------


def operator_home(env_var, dot_dir):
    return Path(os.environ.get(env_var) or (Path.home() / dot_dir))


def cli_launch(engine, prompt, door_url, token, home):
    """argv, env, setup and teardown commands for one CLI pointed at the door
    the way its engine points it. The grant never rides argv; the CLI's
    environment carries no session bearer, only the `plt_` grant."""
    env = {k: v for k, v in os.environ.items() if k != "MAGICIAN_BEARER_TOKEN"}
    setup, teardown = [], []
    if engine == "claude_code":
        config = home / "mcp-config.json"
        config.write_text(json.dumps({"mcpServers": {"magician-plane": {
            "type": "http", "url": door_url, "headers": {"Authorization": f"Bearer {token}"}}}}))
        os.chmod(config, 0o600)
        # The engine reads the final `json` result; the lane wants the tool
        # trace as proof of method, and only the verbose stream carries it.
        argv = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
                "--permission-mode", "bypassPermissions", "--tools", "", "--mcp-config", str(config),
                "--strict-mcp-config", "--setting-sources", ""]
    elif engine == "codex":
        codex_home = home / "codex-home"
        codex_home.mkdir(exist_ok=True)
        auth = operator_home("CODEX_HOME", ".codex") / "auth.json"
        if auth.exists():
            shutil.copyfile(auth, codex_home / "auth.json")
        (codex_home / "config.toml").write_text(
            'sandbox_mode = "read-only"\nweb_search = "disabled"\napproval_policy = "never"\n'
            f'[mcp_servers.{SERVER_NAME}]\nurl = "{door_url}"\nbearer_token_env_var = "{GRANT_ENV_VAR}"\n'
            'default_tools_approval_mode = "approve"\n')
        env["CODEX_HOME"] = str(codex_home)
        env[GRANT_ENV_VAR] = token
        argv = ["codex", "exec", "--json", "--skip-git-repo-check", "-c", 'sandbox_mode="read-only"',
                "--disable", "shell_tool", "--disable", "unified_exec", prompt]
    elif engine == "grok":
        grok_home = home / "grok-home"
        grok_home.mkdir(exist_ok=True)
        auth = operator_home("GROK_HOME", ".grok") / "auth.json"
        if auth.exists():
            shutil.copyfile(auth, grok_home / "auth.json")
        (grok_home / "config.toml").write_text(
            "[compat.claude]\nmcps = false\nhooks = false\nskills = false\nrules = false\nagents = false\n"
            "[compat.cursor]\nmcps = false\nhooks = false\nskills = false\nrules = false\nagents = false\n"
            f'[mcp_servers.{SERVER_NAME}]\nurl = "{door_url}"\nbearer_token_env_var = "{GRANT_ENV_VAR}"\n')
        os.chmod(grok_home / "config.toml", 0o600)
        env["GROK_HOME"] = str(grok_home)
        env[GRANT_ENV_VAR] = token
        argv = ["grok", "-p", prompt, "--output-format", "streaming-messages-json", "--include-partial-messages",
                "--permission-mode", "bypassPermissions", "--no-leader", "--tools", "",
                "--disallowed-tools", GROK_NATIVE_DENYLIST, "--no-subagents", "--disable-web-search"]
    elif engine == "agy":
        setup = [["agy", "mcp", "remove", SERVER_NAME],
                 ["agy", "mcp", "add", "--type", "http", "--header", f"Authorization: Bearer {token}",
                  SERVER_NAME, door_url]]
        teardown = [["agy", "mcp", "remove", SERVER_NAME]]
        argv = ["agy", "-p", prompt, "--output-format", "stream-json", "--sandbox",
                "--disable-slash-commands", "--dangerously-skip-permissions"]
    else:
        raise ValueError(f"no CLI launch for {engine}")
    return {"argv": argv, "env": env, "setup": setup, "teardown": teardown, "cwd": str(home)}


def cli_prompt(nonce):
    # The tool's schema says what it requires (an owning agent id among
    # them); the prompt does not repeat the schema, and does not forbid a
    # second call after a refusal — the effect is the task from Magician's side.
    return ("Use the Magician plane MCP server's create_task tool to create a Magician task titled exactly "
            f"`HC-{nonce}` with the description `plane conformance probe`. "
            "Then reply with the single word DONE.")


def grade_cli(run, title, titles):
    trace = (run.get("stdout") or "") + (run.get("stderr") or "")
    return {
        "cli_exit_ok": run.get("returncode") == 0,
        "task_created": title in titles,
        "via_plane": "create_task" in trace,
    }


def list_task_titles(client, since_ms):
    payload, _ = client.json("GET", "/api/magician/v3/tasks", query={"limit": 200})
    tasks = payload.get("tasks", payload if isinstance(payload, list) else [])
    titles = []
    for row in tasks:
        manifest = row.get("manifest", row) if isinstance(row, dict) else {}
        title = manifest.get("title") or row.get("title") if isinstance(row, dict) else None
        if title:
            titles.append(title)
    return titles


def run_cli_case(args, client, engine, runtime_unavailable, probe_cli):
    nonce = secrets.token_hex(6)
    evidence = {"engine": engine, "case": "cli_door", "repeat": 1, "nonce": nonce, "verdict": "fail",
                "gates": {}, "cleanup_errors": []}
    available, note = probe_cli(engine)
    if not available:
        evidence.update(verdict="cli_unavailable", error=note)
        print(f"  {engine}/cli_door: cli_unavailable ({note})", flush=True)
        return evidence
    home = Path(tempfile.mkdtemp(prefix=f"hc-plane-{engine}-{nonce}-", dir="/tmp"))
    evidence["home"] = str(home)
    grant_id = None
    started_ms, started = int(time.time() * 1000), time.monotonic()
    spec = None
    try:
        identity, _ = client.json("GET", "/api/magician/v2/auth/session")
        grant_id, token = mint_grant(client, f"hc-plane-{engine}-{nonce}", identity["workspace"], "magician",
                                     args.turn_timeout_secs)
        evidence["grant_id"] = grant_id
        door_url = client._url(DOOR_PATH, None)
        spec = cli_launch(engine, cli_prompt(nonce), door_url, token, home)
        evidence["argv"] = [arg if arg != token else "<grant>" for arg in spec["argv"]]
        for command in spec["setup"]:
            done = subprocess.run(command, cwd=spec["cwd"], env=spec["env"], capture_output=True, text=True, timeout=60)
            evidence.setdefault("setup", []).append({"argv": [a if token not in a else "<grant>" for a in command],
                                                     "returncode": done.returncode, "stderr": done.stderr[-500:]})
        print(f"  {engine}/cli_door running {spec['argv'][0]}", flush=True)
        try:
            done = subprocess.run(spec["argv"], cwd=spec["cwd"], env=spec["env"], stdin=subprocess.DEVNULL,
                                  capture_output=True, text=True, timeout=args.turn_timeout_secs)
            run = {"returncode": done.returncode, "stdout": done.stdout[-20000:], "stderr": done.stderr[-4000:]}
        except subprocess.TimeoutExpired as error:
            run = {"returncode": None, "stdout": (error.stdout or b"")[-20000:].decode("utf-8", "replace")
                   if isinstance(error.stdout, bytes) else str(error.stdout or "")[-20000:],
                   "stderr": f"timed out after {args.turn_timeout_secs:.0f} s"}
        evidence["run"] = run
        titles = list_task_titles(client, started_ms)
        evidence["gates"] = grade_cli(run, f"HC-{nonce}", titles)
        evidence["verdict"] = "pass" if all(evidence["gates"].values()) else "fail"
    except runtime_unavailable as error:
        evidence["verdict"] = "inconclusive"
        evidence["error"] = f"Runtime unavailable during evaluation: {error}"
    except Exception as error:
        evidence["error"] = f"{type(error).__name__}: {error}"
    finally:
        if spec:
            for command in spec["teardown"]:
                try:
                    subprocess.run(command, cwd=spec["cwd"], env=spec["env"], capture_output=True, text=True, timeout=60)
                except Exception as error:
                    evidence["cleanup_errors"].append(f"teardown: {error}")
        if grant_id:
            try:
                revoke_grant(client, grant_id)
            except Exception as error:
                evidence["cleanup_errors"].append(f"revoke: {error}")
        if evidence["cleanup_errors"] and evidence["verdict"] not in ("inconclusive", "cli_unavailable"):
            evidence["verdict"] = "fail"
        evidence["latency_ms"] = round((time.monotonic() - started) * 1000)
        retained = args.output_dir / "cases" / nonce
        retained.mkdir(parents=True, exist_ok=True)
        (retained / "evidence.json").write_text(json.dumps(evidence, indent=2, default=str))
    print(f"  {engine}/cli_door: {evidence['verdict']} {evidence['gates']} {evidence.get('error', '')}", flush=True)
    return evidence


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------


def write_report(directory, results, mode):
    directory.mkdir(parents=True, exist_ok=True)
    verdicts = ("pass", "fail", "inconclusive", "cli_unavailable")
    payload = {"lane": "plane", "mode": mode, "results": results,
               "summary": {"ok": bool(results) and all(row["verdict"] == "pass" for row in results),
                           **{verdict: sum(row["verdict"] == verdict for row in results) for verdict in verdicts}},
               "selection": "terminal plt_ grants minted per case and revoked; global settings unchanged"}
    (directory / "report.json").write_text(json.dumps(payload, indent=2, default=str))
    rows = "".join("<tr>" + "".join(f"<td>{html.escape(str(row.get(key, '')))}</td>"
                   for key in ("engine", "case", "verdict", "gates", "error")) + "</tr>" for row in results)
    (directory / "report.html").write_text(
        "<!doctype html><meta charset=utf-8><title>Plane door conformance</title>"
        "<style>body{font:15px system-ui;margin:40px}td,th{text-align:left;padding:10px;border-bottom:1px solid #ddd}</style>"
        "<h1>External harness on the plane MCP door</h1><p>Terminal grants minted per case; tasks retained.</p>"
        "<table><tr><th>Engine</th><th>Case</th><th>Verdict</th><th>Gates</th><th>Error</th></tr>" + rows + "</table>")
    return payload


def main(args, client_type, runtime_unavailable, probe_cli):
    if args.turn_timeout_secs <= 0:
        raise ValueError("plane timeout must be positive")
    if args.self_test:
        import unittest
        import test_eval_harness_plane
        suite = unittest.defaultTestLoader.loadTestsFromModule(test_eval_harness_plane)
        ok = unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful()
        write_report(args.output_dir, [{"engine": "provider-free", "case": "door_contracts",
                                        "verdict": "pass" if ok else "fail", "tests": suite.countTestCases()}], "self-test")
        print("Plane door contracts passed" if ok else "Plane door contracts FAILED")
        return 0 if ok else 1
    wanted = args.engines or [engine for engine, _ in MATRIX]
    unknown = set(wanted) - {engine for engine, _ in MATRIX}
    if unknown:
        raise ValueError(f"plane engines are {[engine for engine, _ in MATRIX]}")
    cases = args.cases or sorted({case for _, case in MATRIX})
    matrix = [(engine, case) for engine, case in MATRIX if engine in wanted and case in cases]
    client = client_type(args.api_base_url, args.http_timeout_secs)
    try:
        login(args, client)
        client.json("GET", "/api/magician/v2/auth/session")
    except runtime_unavailable as error:
        write_report(args.output_dir, [{"engine": engine, "case": case, "repeat": 1, "verdict": "inconclusive",
                                        "error": str(error)} for engine, case in matrix], "live")
        print(f"Runtime unavailable; nothing started. Report: {args.output_dir / 'report.html'}")
        return 2
    results = []
    for engine, case in matrix:
        if case == "scripted_door":
            results.append(run_scripted_case(args, client, runtime_unavailable))
        else:
            results.append(run_cli_case(args, client, engine, runtime_unavailable, probe_cli))
        write_report(args.output_dir, results, "live")
    print(f"Plane report: {args.output_dir / 'report.html'}")
    return 0 if all(row["verdict"] == "pass" for row in results) else 1

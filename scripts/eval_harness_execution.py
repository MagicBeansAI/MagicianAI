"""Effect-based agentic execution checks, using launch-pinned plane grants.

Invoked by eval-harness-conformance-live.py --lane execution. Tests keep their
nonce tasks and fixtures for inspection and revoke only their own grants.
Neither global engine setting is changed — except by `direct_web_research`,
which reruns the web-researcher live eval as a subprocess and must switch the
run engine around it (that eval creates its task through the ordinary V3
path); it restores the saved engine and model in `finally`.
"""
from __future__ import annotations

import html
import json
import os
import re
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from urllib.parse import quote
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

TERMINAL = {"completed", "failed", "cancelled", "canceled"}
CASES = ("local_tools", "local_write", "tool_recovery", "delegate_tools", "direct_web_research")
ENGINES = ("magician", "pi", "claude_code", "codex", "codex_app_server", "grok", "agy")
DEFAULT_DELEGATE = "simple-data-analyst"
# The web-researcher eval's own case the lane reruns per engine, and the
# script that grades it — grounding, citations, judge, lineage — rather than
# a second implementation of those gates here.
WEB_RESEARCH_SCRIPT = Path(__file__).resolve().parent / "eval-web-researcher-live.py"
DEFAULT_WEB_RESEARCH_CASE = "direct_openai_sarvam_pricing"
# The eval proves who decided by the router profile of successful
# `agentic_decision` calls. A harness engine never makes that call — its
# decisions arrive as harness turn settles — so under a harness these two are
# not evidence either way and the lane's settle proof stands in for them.
WEB_RESEARCH_PROFILE_GATES = ("telemetry.eval_decision_profile_observed",
                              "telemetry.eval_decisions_only_use_selected_profile")
# The eval reads tool calls from the analytics fact registry, which a plane
# call inside a harness turn never writes (lane-1 finding #4, still open), so
# these three report zero calls under every harness. The journal is the
# harness's record; the same requirements are read there instead.
WEB_RESEARCH_TOOL_GATES = ("tools.paired_lifecycle", "tools.no_unrecovered_failures",
                           "tools.required_vector_capability")
# The eval's own summary of its gates; the lane re-derives that verdict.
WEB_RESEARCH_SUMMARY_GATES = ("outcome.verdict",)


def login(args, client):
    if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip():
        return
    username = os.environ.get("MAGICIAN_EVAL_USERNAME", "")
    password = os.environ.get("MAGICIAN_EVAL_PASSWORD", "")
    if not username or not password:
        raise RuntimeError("Set MAGICIAN_BEARER_TOKEN or MAGICIAN_EVAL_USERNAME and MAGICIAN_EVAL_PASSWORD")
    status, raw, _ = client.request("POST", "/api/magician/v2/auth/login", body={
        "username": username, "password": password, "workspace": "default",
    })
    # Login returns 201 when it creates the session. Never include the raw
    # auth response in an exception: even a surprising status can carry a token.
    if status not in (200, 201):
        raise RuntimeError(f"evaluation login returned HTTP {status}")
    try:
        payload = json.loads(raw)
    except ValueError:
        raise RuntimeError("evaluation login returned invalid JSON") from None
    bearer = payload.get("token")
    if not isinstance(bearer, str) or not bearer:
        raise RuntimeError("evaluation login did not return a session token")
    os.environ["MAGICIAN_BEARER_TOKEN"] = bearer


class PinnedRun:
    def __init__(self, client, engine, nonce, timeout, runtime_unavailable):
        self.client, self.engine, self.nonce, self.timeout = client, engine, nonce, timeout
        self.runtime_unavailable = runtime_unavailable
        self.grant_id = self.token = self.session = None
        self.rpc_id = 0

    def mint(self, workspace):
        payload, _ = self.client.json("POST", "/api/magician/v2/plane/grants", body={
            "label": f"hc-execution-{self.nonce}", "workspace": workspace,
            "agent_identity": "personal-assistant", "harness_engine": self.engine,
            "ttl_hours": 1, "max_usd": 2.0,
            "max_wall_clock_secs": int(self.timeout), "max_concurrent_runs": 1,
            "allowed_tools": [],
        }, expected=(201,))
        self.grant_id, self.token = payload["grant"]["id"], payload["token"]
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                "clientInfo": {"name": "execution-conformance", "version": "1"}})

    def rpc(self, method, params):
        self.rpc_id += 1
        headers = {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json",
                   "Accept": "application/json, text/event-stream"}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        request = Request(self.client._url("/api/magician/v2/plane/mcp", None),
                          data=json.dumps({"jsonrpc": "2.0", "id": self.rpc_id,
                                           "method": method, "params": params}).encode(), headers=headers)
        try:
            with urlopen(request, timeout=60) as response:
                self.session = response.headers.get("Mcp-Session-Id", self.session)
                raw = response.read(4 * 1024 * 1024 + 1)
        except HTTPError:
            # A responding server's refusal is a failed case, not a restart.
            raise
        except (URLError, TimeoutError, OSError) as error:
            raise self.runtime_unavailable(f"plane {method}: {error}") from error
        if len(raw) > 4 * 1024 * 1024:
            raise RuntimeError("plane response exceeded the evaluation bound")
        try:
            payload = json.loads(raw)
        except ValueError:
            frames = [json.loads(line[5:].strip()) for line in raw.decode().splitlines()
                      if line.startswith("data:")]
            payload = next(row for row in frames if row.get("id") == self.rpc_id)
        if payload.get("error") or payload.get("result", {}).get("isError"):
            raise RuntimeError(f"plane {method} refused: {json.dumps(payload)[:500]}")
        return payload.get("result", {})

    def revoke(self):
        if self.grant_id:
            self.client.json("DELETE", f"/api/magician/v2/plane/grants/{quote(self.grant_id)}",
                             expected=(200, 204))
            self.grant_id = None


def events_for(client, task_id, execution_id, started_ms):
    status, raw, _ = client.request("GET", "/api/magician/v3/events", query={
        "task_id": task_id, "execution_id": execution_id, "since": started_ms,
        "limit": 4000, "backfill_only": "true",
    })
    if status != 200:
        raise RuntimeError(f"execution journal returned HTTP {status}")
    return [json.loads(line) for line in raw.decode().splitlines() if line.strip()]


def governed_hands(engine, events, target_pattern=None):
    """Tool calls the run made through the plane: for a harness engine only the
    calls inside a harness turn (iteration 0) count, for magician every call.
    `target_pattern` narrows to a tool family; None accepts any governed hand."""
    return [row for row in events if row.get("event_type") == "tool.succeeded"
            and (engine == "magician" or row.get("payload", {}).get("iteration") == 0)
            and (target_pattern is None
                 or re.search(target_pattern, str(row.get("payload", {}).get("target", ""))))]


def engine_gates(engine, events):
    """Who thought the run, and whether it was metered — read off the journal.
    A magician run leaves no harness settle; a harness run leaves only its own."""
    settles = [row.get("payload", {}) for row in events
               if row.get("event_type") == "execution.progress"
               and row.get("payload", {}).get("kind") == "harness_turn_settled"]
    proof = not settles if engine == "magician" else bool(settles) and all(
        row.get("engine") == engine for row in settles)
    # A turn Magician itself stopped — for a delegation or an approval — has no
    # bill to report: Magician killed the child before it could settle. Metering
    # is required of every turn that settled on its own.
    billed = [row for row in settles if row.get("stop_reason") not in ("Delegate", "NeedsApproval")]
    metering = engine == "magician" or bool(settles) and all(
        type(row.get("input_tokens")) is int and type(row.get("output_tokens")) is int
        and row["input_tokens"] >= 0 and row["output_tokens"] >= 0
        and row["input_tokens"] + row["output_tokens"] > 0 for row in billed)
    return {"selected_engine": proof, "token_usage": metering,
            "catalog_integrity": not any(row.get("event_type") == "tool.failed"
                and "No provider registered" in str(row.get("payload", {}).get("error", "")) for row in events)}


def grade(engine, status, answer, marker, events, written=None):
    hands = governed_hands(engine, events,
                           r"(?:^|pack:)(?:files(?:__\w+)?|read_file|write_file|grep)(?:$|\()")
    effect = marker in answer and re.search(r"(?<![a-zA-Z0-9])4(?![a-zA-Z0-9])", answer) is not None
    if written is not None:
        effect = effect and written == {"marker": marker, "line_count": 4}
    gates = {"completed": status == "completed", "effect": effect, "governed_tools": bool(hands)}
    gates.update(engine_gates(engine, events))
    return gates


def web_research_verdict(case, gates, require_full=False):
    """The web-researcher eval's own two-half verdict (`assign_verdict`),
    re-run here over the gates the lane admits as evidence. Assertion gates
    fail the case; coverage gates decide full versus partial, and only when
    the runtime itself said the terminal was partial — a terminal that claimed
    to be full and did not cover the request is a failure."""
    if any(not gate.get("passed") and not gate.get("coverage") for gate in gates):
        return "fail"
    if case.get("completion_kind") == "partial":
        if require_full:
            return "fail"
        return "weak_partial" if case.get("honesty") == "resolvable" else "partial_pass"
    if all(gate.get("passed") for gate in gates if gate.get("coverage")):
        return "full_pass"
    return "fail"


def journal_tool_gates(engine, events, fixture, answer_supported=False):
    """The web-researcher eval's three tool gates, read off the run's journal
    for a harness engine: governed hands that succeeded are the paired
    lifecycle, governed hands that failed count against the allowance, and
    the fixture's required tool groups must each name a hand that ran."""
    succeeded = governed_hands(engine, events)
    targets = [str(row.get("payload", {}).get("target", "")) for row in succeeded]
    # The eval's own recovery rule (`unrecovered_tool_failures`): a failed
    # research-pack call is recovered by a later success of the same tool,
    # and a failed read also by an answer the judge found supported through
    # another opened page. Any other failure stands.
    governed = sorted((row for row in events if row.get("event_type") in ("tool.succeeded", "tool.failed")
                       and (engine == "magician" or row.get("payload", {}).get("iteration") == 0)),
                      key=event_time)
    tool_of = lambda row: re.sub(r"^pack:", "", str(row.get("payload", {}).get("target", ""))).split("(")[0]
    any_read = any(row.get("event_type") == "tool.succeeded" and tool_of(row) == "content_read" for row in governed)
    failed = []
    for index, row in enumerate(governed):
        if row.get("event_type") != "tool.failed":
            continue
        tool = tool_of(row)
        later = tool in ("content_search", "content_read") and any(
            other.get("event_type") == "tool.succeeded" and tool_of(other) == tool for other in governed[index + 1:])
        grounded = tool == "content_read" and answer_supported and any_read
        if not (later or grounded):
            failed.append(row)
    groups = fixture.get("required_tool_groups") or []
    missing = [group for group in groups if not any(
        re.search(rf"(?:^|pack:){re.escape(tool)}(?:$|\()", target) for tool in group for target in targets)]
    min_paired = int(fixture.get("min_paired_tool_calls", 1))
    max_failed = int(fixture.get("max_failed_tool_calls", 0))
    return [
        {"name": "tools.paired_lifecycle", "passed": len(succeeded) >= min_paired,
         "detail": f"journal paired={len(succeeded)} required={min_paired}", "coverage": False},
        {"name": "tools.no_unrecovered_failures", "passed": len(failed) <= max_failed,
         "detail": f"journal failed={len(failed)} max={max_failed}", "coverage": False},
        {"name": "tools.required_vector_capability", "passed": not missing,
         "detail": f"journal called={sorted(set(targets))[:12]} missing_groups={missing}", "coverage": False},
    ]


def grade_web_research(engine, report, events, fixture=None):
    """Translate the web-researcher eval's verdict on its one case into the
    lane's gates. The eval's answer, citation, judge, tool and telemetry gates
    ARE the effect; the lane adds who thought the run and whether it was
    metered, read off the same journal as every other case."""
    fixture = fixture or {}
    require_full = bool(fixture.get("require_full", False))
    case = (report.get("cases") or [{}])[0]
    case_ids = {case.get("case_id"), case.get("fixture_id")} - {None}
    own = [gate for gate in report.get("gates", []) if gate.get("case_id") in case_ids
           and gate.get("name") not in WEB_RESEARCH_SUMMARY_GATES]
    profile = [gate for gate in own if gate.get("name") in WEB_RESEARCH_PROFILE_GATES]
    lane = engine_gates(engine, events)
    if engine == "magician":
        # Magician's decisions are router calls on the eval profile; the eval
        # proves that itself, and the journal must show no harness settle.
        # Its runs write tool facts, so its verdict over every gate stands.
        lane["selected_engine"] = lane["selected_engine"] and bool(profile) and all(
            gate.get("passed") for gate in profile)
        verdict = str(case.get("verdict") or "")
        admitted = own
    else:
        # A harness never makes the router call the two profile gates read
        # (the settle proof above says who decided), and its plane calls
        # write no tool facts for the three tool gates to read — the journal
        # carries the same requirements. The verdict is re-derived by the
        # eval's own rule over the gates the lane admits.
        substituted = set(WEB_RESEARCH_PROFILE_GATES) | set(WEB_RESEARCH_TOOL_GATES)
        admitted = [gate for gate in own if gate.get("name") not in substituted]
        admitted += journal_tool_gates(engine, events, fixture, case.get("judge_supported") is True)
        verdict = web_research_verdict(case, admitted, require_full)
    # The verdict carries the eval's rule for a partial terminal. The lane
    # keeps that distinction the way it does for a partial delegation:
    # `effect` says the research happened, governed, on the right engine;
    # `complete_delivery` says it was delivered whole.
    gates = {"completed": case.get("root_status") == "completed",
             "effect": bool(admitted) and verdict in ("full_pass", "partial_pass", "weak_partial"),
             "complete_delivery": verdict == "full_pass",
             "governed_tools": bool(governed_hands(engine, events))}
    gates.update(lane)
    return gates


def web_research_fixture(case_id):
    """The web-researcher eval's fixture entry for `case_id` (its `require_full`
    flag is part of the verdict rule and is not echoed into the report)."""
    fixtures = WEB_RESEARCH_SCRIPT.parent / "fixtures" / "web_researcher_live" / "cases.json"
    try:
        payload = json.loads(fixtures.read_text())
    except (OSError, ValueError):
        return {}
    cases = payload.get("cases", payload) if isinstance(payload, dict) else payload
    return next((case for case in cases if isinstance(case, dict) and case.get("id") == case_id), {})


def web_research_command(args, output_dir):
    """The web-researcher eval, on the running service, for one fixture case.
    Its task is kept (the default): the lane reads the run's journal afterwards
    for the engine proof, and every case of this lane retains its task."""
    return [sys.executable, str(WEB_RESEARCH_SCRIPT),
            "--api-base-url", args.api_base_url,
            "--case", args.execution_web_research_case,
            "--runs", "1",
            "--http-timeout-secs", str(args.http_timeout_secs),
            "--output-dir", str(output_dir)]


def timestamp_ms(value):
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000
    except (AttributeError, ValueError, TypeError):
        return None


def file_calls(events, operation, path, success=True):
    """Match the governed action and fixture path, never model prose."""
    matches = []
    for row in events:
        if row.get("event_type") != ("tool.succeeded" if success else "tool.failed"):
            continue
        target = str(row.get("payload", {}).get("target", ""))
        direct = re.match(r"(?:pack:)?(read_file|write_file|grep)(?:\(|$)", target)
        pack = re.match(r"(?:pack:)?files(?:__(read|write))?(?:\(|$)", target)
        action = direct and ("write" if direct[1] == "write_file" else "read")
        if pack:
            inner = re.search(r'\baction="?(read|write)"?(?=[,)])', target)
            action = pack[1] or (inner[1] if inner else None)
        if action == operation and json.dumps(str(path), ensure_ascii=False) in target:
            matches.append(row)
    return matches


def event_time(row):
    value = row.get("payload", {}).get("timestamp_ms")
    return value if type(value) in (int, float) else timestamp_ms(row.get("timestamp"))


def grade_case(result, marker, source, destination, missing, written=None, delegate=DEFAULT_DELEGATE):
    """Stage-one parity gates. A queued child or a plausible answer cannot pass."""
    engine, case = result["engine"], result["case"]
    task_id, execution_id = result["task_id"], result["execution_id"]
    events = [row for row in result.get("events", [])
              if row.get("task_id") == task_id and row.get("execution_id") == execution_id]
    gates = grade(engine, result["status"], result.get("answer", ""), marker, events, written)
    gates["complete_delivery"] = result.get("completion_kind") != "partial"
    reads = file_calls(events, "read", source)
    writes = file_calls(events, "write", destination)
    if case != "delegate_tools":
        gates["fixture_read"] = bool(reads)
    if case in ("local_write", "delegate_tools"):
        gates["fixture_write"] = bool(writes) and written == {"marker": marker, "line_count": 4}
    if case == "tool_recovery":
        failed = file_calls(events, "read", missing, success=False)
        gates["failed_call_observed"] = bool(failed)
        gates["recovered_after_error"] = any(
            event_time(failure) is not None and event_time(read) is not None
            and event_time(read) > event_time(failure) for failure in failed for read in reads)
    if case == "delegate_tools":
        children = [child for child in result.get("children", [])
                    if child.get("state", {}).get("parent_execution_id") == execution_id
                    and child["state"].get("task_id") == task_id
                    and child["state"].get("relationship_type") == "delegate"
                    and child["state"].get("agent_id") == delegate]
        gates["child_linked"] = bool(children)
        completed = [child for child in children if child["state"].get("status") == "completed"
                     and child["state"].get("completion_kind") != "partial"]
        gates["child_completed"] = bool(completed)
        proved = []
        for child in completed:
            child_events = [row for row in child.get("events", [])
                            if row.get("task_id") == task_id
                            and row.get("execution_id") == child["state"].get("execution_id")]
            if (file_calls(child_events, "read", source)
                    and marker in child.get("answer", "")
                    and re.search(r"\b4\b", child.get("answer", ""))):
                proved.append(child)
        gates["child_tool_and_result"] = bool(proved)
        gates["parent_continued_after_child"] = any(
            timestamp_ms(child["state"].get("completed_at")) is not None
            and event_time(write) is not None
            and event_time(write) >= timestamp_ms(child["state"]["completed_at"])
            for child in proved for write in writes)
        gates["parent_did_not_read_child_input"] = not reads
    return gates


def fixture_prompt(case, source, destination, missing, delegate):
    tools = "Use governed file tools (tool_search can discover files, then files(action=read/write)). "
    write = (f"After obtaining the result, you must write {destination} as JSON with exactly "
             "marker (the second line) and line_count (the numeric line count). ")
    if case == "delegate_tools":
        return (tools + "This is an explicitly requested agent-delegation conformance test. "
                f"You must delegate exactly one child to {delegate} through Magician's delegation "
                f"mechanism. Give the child this task: read {source} through a governed file tool "
                "and return its second line and exact line count. The child must do the read, "
                "return the result, and finish without further delegation or writing the final JSON. "
                "Wait for the child result, then continue as the parent. Do not read the input "
                "yourself or substitute a native CLI subagent or an unrelated task. " + write +
                "Return the marker and count in your final answer. No web or messaging is needed.")
    recovery = (f"First attempt to read {missing} with a governed file tool; the missing-file "
                "error is intentional. After observing that error, recover by reading "
                if case == "tool_recovery" else "Read ")
    return (tools + recovery + f"{source}. Return its second line and exact number of lines. "
            + (write if case == "local_write" else "") +
            "Do this work yourself, without delegation, web access or messaging.")


def read_output(client, task_id, output_id, execution_id=None):
    if not output_id:
        return ""
    base = f"/api/magician/v3/tasks/{quote(task_id)}"
    index_path = base + (f"/executions/{quote(execution_id)}" if execution_id else "") + "/outputs"
    outputs, _ = client.json("GET", index_path)
    output = next((row for row in outputs["outputs"]["outputs"] if row["output_id"] == output_id), None)
    if not output:
        return ""
    code, raw, _ = client.request("GET", base + "/outputs/" + quote(output["relative_path"], safe="/"))
    return raw.decode(errors="replace") if code == 200 else ""


def collect_children(client, task_id, parent_id, started_ms):
    base = f"/api/magician/v3/tasks/{quote(task_id)}"
    payload, _ = client.json("GET", base + "/executions")
    children = []
    for entry in payload["executions"]:
        if entry.get("parent_execution_id") != parent_id:
            continue
        record, _ = client.json("GET", base + "/executions/" + quote(entry["execution_id"]))
        state = record["state"]
        children.append({"state": state,
            "events": events_for(client, task_id, state["execution_id"], started_ms),
            "answer": read_output(client, task_id, state.get("primary_execution_output_id"), state["execution_id"])})
    return children


class GlobalEngineSwitch:
    """The run engine (`execution.harness_engine` and its model) saved, selected
    and restored around the one case that cannot be launch-pinned: the
    web-researcher eval creates its task through the ordinary V3 path, so the
    engine it runs under is the process default. The PUT treats an absent
    `harness_model` as `default`, so the restore sends the saved model too."""

    def __init__(self, client, retry_wait_secs=5.0, retry_window_secs=180.0):
        self.client = client
        self.saved_engine = None
        self.saved_model = None
        self.restore_error = None
        self.retry_wait_secs = retry_wait_secs
        self.retry_window_secs = retry_window_secs

    def save(self):
        payload, _ = self.client.json("GET", "/api/magician/v2/plane/engines")
        self.saved_engine = str(payload.get("current") or "magician")
        self.saved_model = str(payload.get("run_model") or "default")
        return payload

    def select(self, engine):
        self.client.json("PUT", "/api/magician/v2/plane/engine", body={"harness_engine": engine})

    def restore(self):
        """Put the saved engine and model back. The switch is persisted config,
        so a restore that cannot reach the service is not a note in a report,
        it is a runtime left on the wrong engine: measured 2026-09-18, another
        session restarted the service during a case and the run engine stayed
        `grok`. A failed restore waits for the service to answer again and
        retries within a bounded window before it is reported."""
        if self.saved_engine is None:
            return
        body = {"harness_engine": self.saved_engine, "harness_model": self.saved_model}
        deadline = time.monotonic() + self.retry_window_secs
        last = None
        while True:
            try:
                self.client.json("PUT", "/api/magician/v2/plane/engine", body=body)
                self.restore_error = None
                return
            except Exception as error:
                last = f"{type(error).__name__}: {error}"
            # Wait for the service to answer anything before the next attempt.
            while time.monotonic() < deadline:
                time.sleep(self.retry_wait_secs)
                try:
                    self.client.json("GET", "/api/magician/v2/plane/engines")
                    break
                except Exception:
                    continue
            if time.monotonic() >= deadline:
                self.restore_error = last
                return


def run_web_research_case(args, client, engine, repeat, runtime_unavailable):
    """One direct web-researcher case per engine. The web-researcher live eval
    runs as a subprocess under the switched run engine and grades the answer —
    grounding, citations, judge, lineage — with its own gates; this lane reads
    its report and the run's journal for the engine proof."""
    nonce = secrets.token_hex(6)
    case = "direct_web_research"
    retained = args.output_dir / "cases" / nonce
    retained.mkdir(parents=True, exist_ok=True)
    out = retained / "web_researcher"
    result = {"engine": engine, "case": case, "repeat": repeat, "nonce": nonce,
              "fixture_case": args.execution_web_research_case, "verdict": "fail", "gates": {},
              "cleanup_errors": [], "report_dir": str(out)}
    switch = GlobalEngineSwitch(client)
    started_ms, started = int(time.time() * 1000), time.monotonic()
    try:
        roster = switch.save()
        result["saved_engine"] = {"engine": switch.saved_engine, "model": switch.saved_model}
        switch.select(engine)
        selected, _ = client.json("GET", "/api/magician/v2/plane/engines")
        result["selected_engine_reported"] = selected.get("current")
        if selected.get("current") != engine:
            raise RuntimeError(f"run engine did not switch: reported {selected.get('current')!r}")
        command = web_research_command(args, out)
        result["command"] = command
        print(f"  {engine}/{case} running {WEB_RESEARCH_SCRIPT.name} --case {args.execution_web_research_case}", flush=True)
        completed = subprocess.run(command, capture_output=True, text=True,
                                   timeout=max(args.turn_timeout_secs, 60) * 2,
                                   cwd=str(WEB_RESEARCH_SCRIPT.parent.parent), env=dict(os.environ))
        result["subprocess"] = {"returncode": completed.returncode,
                                "stdout_tail": completed.stdout[-4000:], "stderr_tail": completed.stderr[-4000:]}
        report_path = out / "report.json"
        if not report_path.exists():
            raise RuntimeError(f"web-researcher eval wrote no report (exit {completed.returncode})")
        report = json.loads(report_path.read_text())
        cases = report.get("cases") or []
        if len(cases) != 1:
            raise RuntimeError(f"expected one web-researcher case in the report, found {len(cases)}")
        wr_case = cases[0]
        task_id, execution_id = wr_case.get("task_id"), wr_case.get("root_execution_id")
        result["task_id"], result["execution_id"] = task_id, execution_id
        result["status"] = wr_case.get("root_status")
        result["completion_kind"] = wr_case.get("completion_kind")
        result["answer"] = wr_case.get("answer_excerpt", "")
        result["web_researcher"] = {
            "verdict": wr_case.get("verdict"), "citations": wr_case.get("citations", []),
            "gates": [{key: gate.get(key) for key in ("name", "passed", "detail")}
                      for gate in report.get("gates", [])
                      if gate.get("case_id") in {wr_case.get("case_id"), wr_case.get("fixture_id")}],
            "summary": report.get("summary")}
        events = events_for(client, task_id, execution_id, started_ms) if task_id and execution_id else []
        result["events"] = events
        fixture = web_research_fixture(args.execution_web_research_case)
        if engine != "magician":
            result["web_researcher"]["journal_tool_gates"] = journal_tool_gates(
                engine, events, fixture, wr_case.get("judge_supported") is True)
        result["gates"] = grade_web_research(engine, report, events, fixture)
        result["verdict"] = "pass" if all(result["gates"].values()) else "fail"
    except runtime_unavailable as error:
        result["verdict"] = "inconclusive"
        result["error"] = f"Runtime unavailable during evaluation: {error}"
    except subprocess.TimeoutExpired as error:
        result["error"] = f"web-researcher eval exceeded {error.timeout:.0f} s"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        switch.restore()
        if switch.restore_error:
            result["cleanup_errors"].append(f"engine restore: {switch.restore_error}")
        if result["cleanup_errors"] and result["verdict"] != "inconclusive":
            result["verdict"] = "fail"
        result["latency_ms"] = round((time.monotonic() - started) * 1000)
        (retained / "evidence.json").write_text(json.dumps(result, indent=2))
    print(f"  {engine}/{case}: {result['verdict']} {result['gates']} {result.get('error', '')}", flush=True)
    return result


def run_case(args, client, engine, case, repeat, runtime_unavailable):
    if case == "direct_web_research":
        return run_web_research_case(args, client, engine, repeat, runtime_unavailable)
    nonce = secrets.token_hex(6)
    marker = secrets.token_hex(8)
    root = Path(tempfile.mkdtemp(prefix=f"hc-execution-{nonce}-", dir="/tmp"))
    source, destination, missing = root / "input.txt", root / "result.json", root / "missing.txt"
    delegate = args.execution_delegate_agent
    source.write_text(f"amber\n{marker}\ncedar\nplum\n")
    prompt = fixture_prompt(case, source, destination, missing, delegate)
    result = {"engine": engine, "case": case, "repeat": repeat, "nonce": nonce,
              "fixture_dir": str(root), "verdict": "fail", "gates": {}, "cleanup_errors": [],
              "prompt": prompt, "delegate_agent": delegate if case == "delegate_tools" else None}
    pinned = PinnedRun(client, engine, nonce, args.turn_timeout_secs, runtime_unavailable)
    execution_id = task_id = None
    status = "unknown"
    task_state = {}
    started_ms, started = int(time.time() * 1000), time.monotonic()
    try:
        identity, _ = client.json("GET", "/api/magician/v2/auth/session")
        pinned.mint(identity["workspace"])
        result["grant_id"] = pinned.grant_id
        created, _ = client.json("POST", "/api/magician/v3/tasks", body={
            "title": f"[eval/agentic-harness] {engine} {case} {nonce}", "description": prompt,
            "agent_id": "personal-assistant", "approved": True, "created_by": "user",
        }, expected=(201,))
        task_id = created["task"]["manifest"]["task_id"]
        result["task_id"] = task_id
        execution_id = pinned.rpc("tools/call", {"name": "run_task", "arguments": {"task_id": task_id}})["execution_id"]
        result["execution_id"] = execution_id
        print(f"  {engine}/{case} started {execution_id}", flush=True)
        deadline = time.monotonic() + args.turn_timeout_secs
        while time.monotonic() < deadline:
            payload, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}/executions/{quote(execution_id)}")
            state = payload.get("state", {})
            status = state.get("status", "unknown")
            task, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}")
            task_state = task["task"]["state"]
            if status in TERMINAL and not task_state.get("synthesis_pending_executions"):
                break
            if status in {"waiting_for_user", "waiting_for_input", "paused"}:
                break
            time.sleep(2)
        result["status"] = status
        result["completion_kind"] = state.get("completion_kind")
        events = events_for(client, task_id, execution_id, started_ms)
        result["events"] = events
        result["answer"] = read_output(client, task_id, task_state.get("primary_user_output_id"))
        if case == "delegate_tools":
            result["children"] = collect_children(client, task_id, execution_id, started_ms)
        written = None
        if case in ("local_write", "delegate_tools"):
            try:
                written = json.loads(destination.read_text())
            except (OSError, ValueError):
                written = {}
        result["written"] = written
        result["gates"] = grade_case(result, marker, source, destination, missing, written, delegate)
        result["verdict"] = "pass" if all(result["gates"].values()) else "fail"
    except runtime_unavailable as error:
        result["verdict"] = "inconclusive"
        result["error"] = f"Runtime unavailable during evaluation: {error}"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        if execution_id and status not in TERMINAL:
            try:
                client.json("POST", f"/api/magician/v3/executions/{quote(execution_id)}/cancel", body={}, expected=(200, 202, 409))
                result["cancel_requested"] = True
                cancel_deadline = time.monotonic() + 30
                while time.monotonic() < cancel_deadline:
                    payload, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}/executions/{quote(execution_id)}")
                    cleanup_status = payload.get("state", {}).get("status")
                    if cleanup_status in TERMINAL:
                        result["cleanup_status"] = cleanup_status
                        break
                    time.sleep(1)
                else:
                    result["cleanup_errors"].append("execution did not settle within 30 seconds of cancellation")
            except Exception as error:
                result["cleanup_errors"].append(f"cancel: {error}")
        # A root can fail or claim completion while a delegate remains active.
        # Re-read the task-owned index, including descendants, before revocation.
        if task_id and case == "delegate_tools":
            try:
                payload, _ = client.json("GET", f"/api/magician/v3/tasks/{quote(task_id)}/executions")
                owned = {execution_id}
                for _ in range(len(payload["executions"])):
                    owned.update(row["execution_id"] for row in payload["executions"]
                                 if row.get("parent_execution_id") in owned)
                for row in payload["executions"]:
                    if row["execution_id"] in owned - {execution_id} and row["status"] not in TERMINAL:
                        client.json("POST", f"/api/magician/v3/executions/{quote(row['execution_id'])}/cancel",
                                    body={}, expected=(200, 202, 409))
                        result["cleanup_errors"].append(f"child still active at cleanup: {row['execution_id']}; cancellation requested")
            except Exception as error:
                result["cleanup_errors"].append(f"child cleanup: {error}")
        try:
            pinned.revoke()
        except Exception as error:
            result["cleanup_errors"].append(f"revoke: {error}")
        if result["cleanup_errors"] and result["verdict"] != "inconclusive":
            result["verdict"] = "fail"
        result["latency_ms"] = round((time.monotonic() - started) * 1000)
        (root / "evidence.json").write_text(json.dumps(result, indent=2))
        retained = args.output_dir / "cases" / nonce
        retained.mkdir(parents=True, exist_ok=True)
        (retained / "evidence.json").write_text(json.dumps(result, indent=2))
    print(f"  {engine}/{case}: {result['verdict']} {result['gates']} {result.get('error', '')}", flush=True)
    return result


def write_report(directory, results, mode):
    directory.mkdir(parents=True, exist_ok=True)
    payload = {"lane": "execution", "mode": mode, "results": results,
               "summary": {"ok": bool(results) and all(row["verdict"] == "pass" for row in results),
                           **{verdict: sum(row["verdict"] == verdict for row in results)
                              for verdict in ("pass", "fail", "inconclusive")}},
               "selection": "launch-pinned grants; global settings unchanged"}
    (directory / "report.json").write_text(json.dumps(payload, indent=2))
    rows = "".join("<tr>" + "".join(f"<td>{html.escape(str(row.get(key, '')))}</td>"
                  for key in ("engine", "case", "verdict", "gates", "error")) + "</tr>" for row in results)
    (directory / "report.html").write_text("<!doctype html><meta charset=utf-8><title>Execution harness conformance</title>"
        "<style>body{font:15px system-ui;margin:40px}td,th{text-align:left;padding:10px;border-bottom:1px solid #ddd}</style>"
        "<h1>Agentic execution harness conformance</h1><p>Launch-pinned runs; retained fixture and task evidence.</p>"
        "<table><tr><th>Engine</th><th>Case</th><th>Verdict</th><th>Gates</th><th>Error</th></tr>" + rows + "</table>")
    return payload


def main(args, client_type, runtime_unavailable):
    if args.turn_timeout_secs <= 0:
        raise ValueError("execution timeout must be positive")
    if args.self_test:
        import unittest
        import test_eval_harness_execution
        suite = unittest.defaultTestLoader.loadTestsFromModule(test_eval_harness_execution)
        if not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful():
            write_report(args.output_dir, [{"engine": "provider-free", "case": "stage_one_grading", "verdict": "fail"}], "self-test")
            return 1
        from unittest.mock import Mock, patch
        with patch.dict(os.environ, {"MAGICIAN_BEARER_TOKEN": "", "MAGICIAN_EVAL_USERNAME": "fixture", "MAGICIAN_EVAL_PASSWORD": "fixture"}):
            for status in (200, 201):
                os.environ["MAGICIAN_BEARER_TOKEN"] = ""
                auth = Mock()
                auth.request.return_value = (status, b'{"token":"fixture-only-token"}', 0)
                login(args, auth)
                assert os.environ["MAGICIAN_BEARER_TOKEN"] == "fixture-only-token"
            os.environ["MAGICIAN_BEARER_TOKEN"] = ""
            auth.request.return_value = (403, b'{"token":"must-not-be-logged"}', 0)
            try:
                login(args, auth)
            except RuntimeError as error:
                assert "403" in str(error) and "must-not-be-logged" not in str(error)
            else:
                raise AssertionError("a login refusal must fail without exposing its response")
        probe = PinnedRun(client_type("http://127.0.0.1:1", 1), "codex", "probe", 1, runtime_unavailable)
        for failure in (URLError("connection reset"), TimeoutError("read interrupted")):
            with patch(__name__ + ".urlopen", side_effect=failure):
                try:
                    probe.rpc("initialize", {})
                except runtime_unavailable:
                    pass
                else:
                    raise AssertionError("an interrupted MCP request must be inconclusive")
        with patch(__name__ + ".urlopen", side_effect=HTTPError("http://127.0.0.1:1", 403, "denied", {}, None)):
            try:
                probe.rpc("initialize", {})
            except HTTPError:
                pass
            else:
                raise AssertionError("a responding server's denial must stay a failure")
        events = [{"event_type": "tool.succeeded", "payload": {"iteration": 0, "target": "pack:files(action=read)"}},
                  {"event_type": "execution.progress", "payload": {"kind": "harness_turn_settled",
                      "engine": "codex", "input_tokens": 10, "output_tokens": 2}}]
        assert all(grade("codex", "completed", "nonce: 4 lines", "nonce", events).values())
        assert not all(grade("codex", "failed", "nonce: 4 lines", "nonce", events).values())
        assert not grade("grok", "completed", "nonce: 4 lines", "nonce", events)["selected_engine"]
        assert not grade("codex", "completed", "nonce: 4 lines", "nonce", events[:1])["token_usage"]
        assert not grade("codex", "completed", "nonce: 4 lines", "nonce", events, {})["effect"]
        assert not grade("codex", "completed", "nonce: 4 lines", "nonce", events[1:])["governed_tools"]
        broken = {"event_type": "tool.failed", "payload": {"error": "No provider registered for pack capability 'read_file'"}}
        assert not grade("codex", "completed", "nonce: 4 lines", "nonce", events + [broken])["catalog_integrity"]
        write_report(args.output_dir, [{"engine": "provider-free", "case": "grading", "verdict": "pass",
                                      "tests": suite.countTestCases()}], "self-test")
        print("Execution grading contracts passed")
        return 0
    cases = args.cases or list(CASES)
    if set(cases) - set(CASES):
        raise ValueError(f"execution cases are {CASES}")
    engines = args.engines or list(ENGINES)
    if set(engines) - set(ENGINES):
        raise ValueError(f"execution engines are {ENGINES}")
    client = client_type(args.api_base_url, args.http_timeout_secs)
    try:
        login(args, client)
        roster, _ = client.json("GET", "/api/magician/v2/plane/engines")
    except runtime_unavailable as error:
        write_report(args.output_dir, [{"engine": engine, "case": case, "repeat": repeat,
            "verdict": "inconclusive", "error": str(error)} for engine in engines
            for repeat in range(1, args.runs + 1) for case in cases], "live")
        print(f"Runtime unavailable; no task started. Report: {args.output_dir / 'report.html'}")
        return 2
    installed = [row["name"] for row in roster["engines"] if row["installed"]]
    results = []
    for engine in engines:
        for repeat in range(1, args.runs + 1):
            for case in cases:
                if engine not in installed:
                    results.append({"engine": engine, "case": case, "repeat": repeat,
                                    "verdict": "inconclusive", "error": "Engine is not installed"})
                else:
                    results.append(run_case(args, client, engine, case, repeat, runtime_unavailable))
                write_report(args.output_dir, results, "live")
    print(f"Execution report: {args.output_dir / 'report.html'}")
    return 0 if all(row["verdict"] == "pass" for row in results) else 1

#!/usr/bin/env python3
"""Boundary B's router, measured on the real web-researcher: lane off, lane on.

The deterministic suite proves the working-set lane recovers planted facts;
the live evaluator proves a model answers from the lane's bytes; neither
proves that a real research task, routed by the real rule at the real seam,
comes out at least as well and cheaper. This does. It runs the web-researcher
live evaluation's case twice per repeat — once with the lane closed, once
open — by editing the live config's `enabled_lanes` and asking the runtime to
reload it (the same snapshot `content_read` consults), and compares:

  * the child evaluation's own verdict and gates, which must not regress;
  * whether the routing rule actually activated on the lane-on runs, read
    off the execution index the runtime writes — and if it did not, the
    sentence that says why, so a case that never crosses a threshold is
    reported as that rather than hidden;
  * input tokens and cost from the child's own `llm_calls` rows;
  * whether the model used the path: an execution-scoped
    `working_set_search` call on a lane-on run.

The original config bytes are restored in `finally`, Ctrl-C included, and the
runtime is asked to reload them; a restore that fails is printed last and
loudest. The lane-off arm is the file with the gate shut, not the file as
found: once a lane is open for good, the operator's file *is* the on arm.
`--as-configured` writes nothing — it runs the case against the file as the
operator left it and checks the runtime routes the way the file says, which
is the check to run right after opening (or closing) a lane. `--self-test`
pins the config surgery, the gate reader, the report contract and the gate
arithmetic on canned payloads without a runtime.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

DEFAULT_BASE_URL = "http://127.0.0.1:3002"
DEFAULT_OUTPUT_DIR = Path("coverage/evals/working-set-routing/live/latest")
DEFAULT_CASE = "direct_openai_sarvam_pricing"
DEFAULT_LANE = "web-research"
WEB_RESEARCHER_EVAL = Path(__file__).with_name("eval-web-researcher-live.py")
PASSING_VERDICTS = ("full_pass", "partial_pass")


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------


class Client:
    def __init__(self, base_url: str, timeout: float = 60.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def request(self, method: str, path: str, body: Any = None, query: dict | None = None):
        url = self.base_url + path
        if query:
            url += "?" + urlencode(query)
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        request.add_header("Content-Type", "application/json")
        token = os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
        if token:
            request.add_header("Authorization", f"Bearer {token}")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            return error.code, error.read()

    def json(self, method: str, path: str, body: Any = None, query: dict | None = None):
        status, raw = self.request(method, path, body, query)
        try:
            return status, json.loads(raw) if raw else None
        except json.JSONDecodeError:
            return status, {"raw": raw.decode(errors="replace")}


def login(client: Client) -> None:
    """A bearer from the environment, else a session login — the same rule as
    the other lanes. The token is exported so the child inherits it."""
    if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip():
        return
    username = os.environ.get("MAGICIAN_EVAL_USERNAME", "").strip()
    password = os.environ.get("MAGICIAN_EVAL_PASSWORD", "")
    if not username:
        raise RuntimeError("set MAGICIAN_BEARER_TOKEN or MAGICIAN_EVAL_USERNAME/MAGICIAN_EVAL_PASSWORD")
    status, payload = client.json("POST", "/api/magician/v2/auth/login",
                                  {"username": username, "password": password})
    if status not in (200, 201) or not isinstance(payload, dict) or not payload.get("token"):
        raise RuntimeError(f"login refused with HTTP {status}")
    os.environ["MAGICIAN_BEARER_TOKEN"] = payload["token"]


# ---------------------------------------------------------------------------
# The lane switch: config bytes in, reload, config bytes back
# ---------------------------------------------------------------------------


def default_config_path() -> Path:
    root = os.environ.get("MAGICIAN_ROOT_DIR", "").strip() or str(Path.home() / "MagicianNotes")
    return Path(root) / "magician-config.yaml"


def default_data_root() -> Path:
    root = os.environ.get("MAGICIAN_ROOT_DIR", "").strip() or str(Path.home() / "MagicianNotes")
    return Path(root)


def config_with_lanes_enabled(original: str, enabled: list[str]) -> str:
    """The original config with `enabled_lanes` set to exactly `enabled` under
    `content_acquisition.working_sets.activation`, and nothing else moved.
    Text surgery on purpose: a YAML round-trip would rewrite the whole live
    file and drop its comments. Whatever the operator left — an open list, a
    closed inline list, or no activation block at all — the result states the
    gate in one inline line and keeps the `lanes` map beside it, because the
    runtime reads both and the map is what says which agents the lane holds."""
    gate_line = f"      enabled_lanes: [{', '.join(enabled)}]"
    lines = original.split("\n")
    out: list[str] = []
    i = 0
    inserted = False
    while i < len(lines):
        line = lines[i]
        out.append(line)
        if not inserted and re.match(r"^  working_sets:\s*$", line):
            j = i + 1
            block: list[str] = []
            while j < len(lines) and (lines[j].startswith("    ") or lines[j].strip() == ""):
                block.append(lines[j])
                j += 1
            # Trailing blank lines belong after the block, not inside it.
            trailing: list[str] = []
            while block and block[-1].strip() == "":
                trailing.insert(0, block.pop())
            kept: list[str] = []
            k = 0
            gate_written = False
            while k < len(block):
                if re.match(r"^    activation:\s*$", block[k]):
                    kept.append(block[k])
                    k += 1
                    # Inside the activation block: replace the gate entry (inline
                    # or a dash list), keep every other entry as written.
                    while k < len(block) and (block[k].startswith("      ") or block[k].strip() == ""):
                        if re.match(r"^      enabled_lanes:", block[k]):
                            k += 1
                            while k < len(block) and (block[k].startswith("        ") or block[k].strip() == ""):
                                k += 1
                            if not gate_written:
                                kept.append(gate_line)
                                gate_written = True
                            continue
                        kept.append(block[k])
                        k += 1
                    if not gate_written:
                        # A block with no gate entry: state it first.
                        insert_at = len(kept)
                        while insert_at > 0 and kept[insert_at - 1].strip() == "":
                            insert_at -= 1
                        head = kept.index("    activation:") + 1
                        kept.insert(head, gate_line)
                        gate_written = True
                    continue
                kept.append(block[k])
                k += 1
            out.extend(kept)
            if not gate_written:
                out.append("    activation:")
                out.append(gate_line)
            out.extend(trailing)
            inserted = True
            i = j
            continue
        i += 1
    if not inserted:
        raise RuntimeError("could not find `  working_sets:` under content_acquisition in the live config")
    return "\n".join(out)


def config_with_lane(original: str, lane: str) -> str:
    """The lane-on arm: the gate holds exactly `lane`."""
    return config_with_lanes_enabled(original, [lane])


def config_with_lane_closed(original: str) -> str:
    """The lane-off arm: the gate is stated and empty. The operator's file may
    already be open — it is, once a lane has been opened for good — so the
    baseline is not the file as found but the file with the gate shut."""
    return config_with_lanes_enabled(original, [])


def enabled_lanes_in(config_text: str) -> list[str]:
    """The gate as the operator left it: the `enabled_lanes` entry under
    `content_acquisition.working_sets.activation`, inline or as a dash list.
    No block, or no entry, is a closed gate — the runtime's default."""
    lines = config_text.split("\n")
    in_working_sets = in_activation = False
    i = 0
    while i < len(lines):
        line = lines[i]
        if re.match(r"^  working_sets:\s*$", line):
            in_working_sets = True
        elif in_working_sets and line.strip() and not line.startswith("    "):
            return []
        elif in_working_sets and re.match(r"^    activation:\s*$", line):
            in_activation = True
        elif in_activation and line.strip() and not line.startswith("      "):
            return []
        elif in_activation:
            inline = re.match(r"^      enabled_lanes:\s*\[(.*)\]\s*$", line)
            if inline:
                return [item.strip().strip("'\"") for item in inline.group(1).split(",") if item.strip()]
            if re.match(r"^      enabled_lanes:\s*$", line):
                lanes: list[str] = []
                i += 1
                while i < len(lines) and (lines[i].startswith("        ") or lines[i].strip() == ""):
                    item = re.match(r"^        -\s*(.+?)\s*$", lines[i])
                    if item:
                        lanes.append(item.group(1).strip("'\""))
                    i += 1
                return lanes
        i += 1
    return []


def grade_as_configured(runs: list[dict[str, Any]], lane: str, lane_open: bool) -> list[dict[str, Any]]:
    """The one question after an operator edits the gate: does the runtime
    decide the way the file says? Every run must finish. With the lane open,
    every run must have been decided under it with the gate open — activated
    when the task's scale qualifies, refused below the threshold, never
    "gate shut". With the lane shut, no run may activate or see an open gate."""
    gates: list[dict[str, Any]] = []

    def gate(name, passed, detail):
        gates.append({"name": name, "passed": bool(passed), "detail": detail})

    finished = [run for run in runs if run.get("verdict") is not None]
    gate("every_run_finished", len(finished) == len(runs) and runs,
         f"{len(finished)}/{len(runs)} run(s) produced a verdict"
         + ("" if len(finished) == len(runs) else "; errors: " + "; ".join(str(r.get("error")) for r in runs if r.get("verdict") is None)))
    if lane_open:
        under_lane = [run for run in runs if run.get("gate_open") is True and run.get("decided_lane") == lane]
        activated = [run for run in under_lane if run.get("activated")]
        gate("routing_matches_config", len(under_lane) == len(runs) and runs,
             f"file opens `{lane}`; {len(under_lane)}/{len(runs)} run(s) decided under the open lane, "
             f"{len(activated)} activated"
             + ("" if len(under_lane) == len(runs) else " — a run was not decided under the open lane: " + "; ".join(
                 str(run.get("decision_reason") or run.get("error") or "no decision recorded")
                 for run in runs if run not in under_lane))
             + ("" if not under_lane else "; reasons: " + "; ".join(
                 str(run.get("decision_reason")) for run in under_lane)))
    else:
        open_or_active = [run for run in runs if run.get("activated") or run.get("gate_open") is True]
        gate("routing_matches_config", not open_or_active,
             f"file keeps `{lane}` shut; {len(open_or_active)}/{len(runs)} run(s) activated or saw an open gate"
             + ("" if not open_or_active else " — a shut gate was open at the runtime"))
    return gates


def reload_runtime_config(client: Client) -> None:
    status, payload = client.json("POST", "/api/magician/v2/settings/magician-config/reload", {})
    if status != 200:
        raise RuntimeError(f"config reload refused with HTTP {status}: {payload}")


# ---------------------------------------------------------------------------
# Evidence
# ---------------------------------------------------------------------------


def execution_index_for(data_root: Path, execution_ids: list[str]) -> dict[str, Any] | None:
    """The runtime's own record of what it decided, wherever the scope lives."""
    for execution_id in execution_ids:
        for path in data_root.glob(f"scopes/*/*/research/working_set_executions/{execution_id}.json"):
            try:
                return json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                continue
    return None


def summarize_case(case: dict[str, Any], index: dict[str, Any] | None) -> dict[str, Any]:
    llm_calls = case.get("llm_calls") or []
    tool_calls = case.get("tool_calls") or []
    activation = (index or {}).get("activation")
    decision = (index or {}).get("decision")
    return {
        "verdict": case.get("verdict"),
        "task_id": case.get("task_id"),
        "root_execution_id": case.get("root_execution_id"),
        "citations": len(case.get("citations") or []),
        "answer_chars": case.get("answer_chars"),
        "input_tokens": sum(int(call.get("input_tokens") or 0) for call in llm_calls),
        "cost_usd": round(sum(float(call.get("cost_usd") or 0.0) for call in llm_calls), 6),
        "llm_calls": len(llm_calls),
        "content_reads": sum(1 for call in tool_calls if call.get("tool_name") in ("content_read", "web_fetch")),
        "working_set_searches": sum(1 for call in tool_calls if call.get("tool_name") == "working_set_search"),
        "working_set_reads": sum(1 for call in tool_calls if call.get("tool_name") == "working_set_read"),
        "index": None if index is None else {
            "members": len(index.get("members") or []),
            "total_source_bytes": index.get("total_source_bytes"),
            "distinct_sources": index.get("distinct_sources"),
            "read_rounds": index.get("read_rounds"),
        },
        "activated": bool(activation),
        "activation_lane": (activation or {}).get("lane"),
        "activation_reason": (activation or {}).get("reason"),
        # The last decision either way, as the runtime kept it: a refusal
        # says whether the gate was shut or the task was below the threshold.
        "decided_lane": (decision or {}).get("lane"),
        "gate_open": (decision or {}).get("gate_open"),
        "decision_reason": (decision or {}).get("reason"),
        "beyond_window_bytes": (index or {}).get("beyond_window_bytes"),
        "narrowed_reads": (index or {}).get("narrowed_reads", 0),
        "error": case.get("error"),
    }


def grade(off: list[dict[str, Any]], on: list[dict[str, Any]]) -> list[dict[str, Any]]:
    def passing(runs):
        return sum(1 for run in runs if run.get("verdict") in PASSING_VERDICTS)

    def mean(runs, key):
        values = [float(run.get(key) or 0) for run in runs]
        return sum(values) / len(values) if values else 0.0

    gates: list[dict[str, Any]] = []

    def gate(name, passed, detail):
        gates.append({"name": name, "passed": bool(passed), "detail": detail})

    gate("both_arms_ran", bool(off) and bool(on) and all(run.get("verdict") for run in off + on),
         f"lane-off {len(off)} run(s), lane-on {len(on)} run(s)")
    gate("no_regression", passing(on) >= passing(off),
         f"passing verdicts: lane-on {passing(on)}/{len(on)} vs lane-off {passing(off)}/{len(off)}")
    activated_on = [run for run in on if run.get("activated")]
    not_activated = [run for run in on if not run.get("activated")]
    under_lane = [run for run in on if run.get("gate_open") is True]
    activated_on = [run for run in on if run.get("activated")]
    gate("lane_open_when_open", len(under_lane) == len(on) and on,
         f"every lane-on run decided under the open lane; {len(activated_on)}/{len(on)} activated"
         + ("" if len(under_lane) == len(on) else " — a lane-on run saw the gate shut: " + "; ".join(
             str(run.get("decision_reason") or run.get("error") or "no decision") for run in on if run not in under_lane))
         + ("" if not on else "; reasons: " + "; ".join(
             str(run.get("activation_reason") or run.get("decision_reason")) for run in on)))
    gate("routing_closed_when_shut", not any(run.get("activated") or run.get("gate_open") is True for run in off),
         "no lane-off run activated or saw an open gate" if not any(run.get("activated") or run.get("gate_open") is True for run in off)
         else "a lane-off run activated or saw an open gate — the gate is not closed")
    # Narrowing is non-lossy: only a page larger than the ordinary window is
    # shown by its head. On a case whose pages all fit, the path takes nothing
    # away and there is nothing for the model to go and get back — so "used
    # the path" is only a claim on runs that narrowed something. Such a run
    # must search; a case that never narrows is reported as not exercising
    # the path, which is a fact about the case, not a pass or a fail.
    narrowed_on = [run for run in activated_on if (run.get("narrowed_reads") or 0) > 0]
    if narrowed_on:
        gate("model_used_the_path_when_narrowed",
             all(run.get("working_set_searches", 0) > 0 for run in narrowed_on),
             f"narrowed runs and their execution-scoped searches: "
             f"{[(run.get('narrowed_reads'), run.get('working_set_searches', 0)) for run in narrowed_on]}")
    else:
        gate("model_used_the_path_when_narrowed", True,
             "no lane-on run narrowed a read (every page fit the ordinary window): the path was "
             "opened but never exercised on this case — the fixture live eval, not this A/B, is "
             "the evidence for what it does on large pages")
    # Safety, not benefit: the lane must not make a task dearer beyond noise.
    # The benefit — fewer bytes and a lower bill on large pages — is what the
    # fixture live eval measures; this case's pages are what they are.
    noise = 1.10
    gate("not_dearer_when_open",
         bool(on) and bool(off) and mean(on, "cost_usd") <= mean(off, "cost_usd") * noise,
         f"mean cost lane-on ${mean(on, 'cost_usd'):.4f} vs lane-off ${mean(off, 'cost_usd'):.4f} "
         f"(allowed up to {noise:.0%} of lane-off)")
    gate("not_more_input_tokens_when_open",
         bool(on) and bool(off) and mean(on, "input_tokens") <= mean(off, "input_tokens") * noise,
         f"mean input tokens lane-on {mean(on, 'input_tokens'):.0f} vs lane-off {mean(off, 'input_tokens'):.0f}")
    return gates


# ---------------------------------------------------------------------------
# Driving the child evaluation
# ---------------------------------------------------------------------------


def run_child(args, arm: str, run_index: int) -> dict[str, Any]:
    output_dir = args.output_dir / arm / f"run-{run_index}"
    output_dir.mkdir(parents=True, exist_ok=True)
    command = [
        sys.executable, str(WEB_RESEARCHER_EVAL),
        "--case", args.case,
        "--runs", "1",
        "--output-dir", str(output_dir),
        "--http-timeout-secs", str(args.http_timeout_secs),
    ]
    started = time.time()
    completed = subprocess.run(command, capture_output=True, text=True, env=os.environ.copy())
    (output_dir / "child.stdout").write_text(completed.stdout, encoding="utf-8")
    (output_dir / "child.stderr").write_text(completed.stderr, encoding="utf-8")
    report_path = output_dir / "report.json"
    if not report_path.exists():
        return {"verdict": None, "error": f"child wrote no report (exit {completed.returncode})",
                "elapsed_s": round(time.time() - started, 1)}
    report = json.loads(report_path.read_text(encoding="utf-8"))
    cases = report.get("cases") or []
    case = next((c for c in cases if c.get("case_id") == args.case), cases[0] if cases else {})
    execution_ids = [case.get("root_execution_id") or ""] + list(case.get("execution_ids") or [])
    index = execution_index_for(args.data_root, [e for e in execution_ids if e])
    summary = summarize_case(case, index)
    summary["elapsed_s"] = round(time.time() - started, 1)
    summary["child_exit"] = completed.returncode
    return summary


def write_report(output_dir: Path, payload: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    rows = []
    for arm in ("off", "on"):
        for i, run in enumerate(payload["runs"].get(arm) or []):
            rows.append(
                f"<tr><td>{arm}</td><td>{i + 1}</td><td>{html.escape(str(run.get('verdict')))}</td>"
                f"<td>{run.get('activated')}</td><td>{run.get('content_reads')}</td>"
                f"<td>{run.get('working_set_searches')}</td><td>{run.get('input_tokens')}</td>"
                f"<td>${run.get('cost_usd')}</td><td>{run.get('citations')}</td>"
                f"<td>{html.escape(str(run.get('activation_reason') or run.get('error') or ''))}</td></tr>")
    gates = "".join(
        f"<li>{'PASS' if g['passed'] else 'FAIL'} <b>{html.escape(g['name'])}</b>: {html.escape(g['detail'])}</li>"
        for g in payload["gates"])
    (output_dir / "report.html").write_text(
        "<!doctype html><meta charset='utf-8'><title>Working-set routing A/B</title>"
        "<h1>Working-set routing on the web-researcher: lane off vs on</h1>"
        f"<p>case <code>{html.escape(payload['case'])}</code>, lane <code>{html.escape(payload['lane'])}</code>, "
        f"mode {payload['mode']}, {'PASSED' if payload['passed'] else 'FAILED'}</p>"
        f"<ul>{gates}</ul>"
        "<table border=1 cellpadding=4><tr><th>arm</th><th>run</th><th>verdict</th><th>activated</th>"
        "<th>reads</th><th>ws searches</th><th>input tokens</th><th>cost</th><th>citations</th><th>reason / error</th></tr>"
        f"{''.join(rows)}</table><p><a href='report.json'>Raw JSON</a></p>",
        encoding="utf-8")


OPEN_CONFIG = """content_acquisition:
  # comment above the block
  working_sets:
    auto_capture_agents:
      - web-researcher
    # `enabled_lanes` is the gate.
    activation:
      enabled_lanes:
        - web-research
      lanes:
        web-research:
          - web-researcher
  progressive_retrieval:
    enabled: true
"""

CLOSED_INLINE_CONFIG = OPEN_CONFIG.replace("      enabled_lanes:\n        - web-research\n", "      enabled_lanes: []\n")

BARE_CONFIG = """content_acquisition:
  working_sets:
    auto_capture_agents:
      - web-researcher

  progressive_retrieval:
    enabled: true
"""


def self_test_config_surgery() -> list[str]:
    """The surgery must set the gate and nothing else, whatever the operator
    left in the file: an open list, a closed inline list, or no block at all.
    Returns the failures; empty means the contract holds."""
    failures: list[str] = []

    def expect(name: str, ok: bool) -> None:
        if not ok:
            failures.append(name)

    off = config_with_lanes_enabled(OPEN_CONFIG, [])
    expect("open original: the off arm closes the gate", "      enabled_lanes: []\n" in off)
    expect("open original: the off arm keeps the lanes map", "        web-research:\n          - web-researcher\n" in off)
    expect("open original: the off arm keeps the comments", "    # `enabled_lanes` is the gate.\n" in off)
    expect("open original: the off arm keeps the sibling block", "  progressive_retrieval:\n    enabled: true\n" in off)
    expect("open original: the off arm drops the open list", "        - web-research\n" not in off)
    expect("open original: one activation block", off.count("    activation:") == 1)

    on = config_with_lanes_enabled(OPEN_CONFIG, ["web-research"])
    expect("open original: the on arm opens exactly the lane", "      enabled_lanes: [web-research]\n" in on)
    expect("open original: the on arm keeps the lanes map", "        web-research:\n          - web-researcher\n" in on)
    expect("open original: the on arm is idempotent", config_with_lanes_enabled(on, ["web-research"]) == on)

    reopened = config_with_lanes_enabled(CLOSED_INLINE_CONFIG, ["web-research"])
    expect("closed inline original: the on arm replaces the inline list", "      enabled_lanes: [web-research]\n" in reopened
           and "      enabled_lanes: []\n" not in reopened)

    bare_on = config_with_lanes_enabled(BARE_CONFIG, ["web-research"])
    expect("no block: the on arm adds one", "    activation:\n      enabled_lanes: [web-research]\n" in bare_on)
    expect("no block: the block lands inside working_sets", bare_on.index("    activation:") < bare_on.index("  progressive_retrieval:"))
    bare_off = config_with_lanes_enabled(BARE_CONFIG, [])
    expect("no block: the off arm states the closed gate", "    activation:\n      enabled_lanes: []\n" in bare_off)

    expect("config_with_lane is the on arm", config_with_lane(OPEN_CONFIG, "web-research") == on)

    # The as-configured mode reads the gate the operator left, in either spelling.
    expect("reads an open dash list", enabled_lanes_in(OPEN_CONFIG) == ["web-research"])
    expect("reads a closed inline list", enabled_lanes_in(CLOSED_INLINE_CONFIG) == [])
    expect("reads an open inline list", enabled_lanes_in(on) == ["web-research"])
    expect("no block means closed", enabled_lanes_in(BARE_CONFIG) == [])

    # ...and its one gate: the runtime must decide the way the file says.
    # An open lane means every run is decided under it — activated when the
    # task's scale says so, refused below the threshold — never "gate shut".
    activated = {"verdict": "partial_pass", "activated": True, "decided_lane": "web-research", "gate_open": True,
                 "decision_reason": "24576 bytes beyond the window meet the 24576-byte threshold"}
    below = {"verdict": "partial_pass", "activated": False, "decided_lane": "web-research", "gate_open": True,
             "decision_reason": "below every activation threshold (11264 of 24576 bytes beyond the window)"}
    shut = {"verdict": "partial_pass", "activated": False, "decided_lane": "web-research", "gate_open": False,
            "decision_reason": "lane `web-research` has no working-set evaluation evidence yet; the routing gate is closed"}
    undecided = {"verdict": "partial_pass", "activated": False}
    expect("open file + activated run passes", all(g["passed"] for g in grade_as_configured([activated], "web-research", True)))
    expect("open file + a run below the threshold passes", all(g["passed"] for g in grade_as_configured([below, activated], "web-research", True)))
    expect("open file + a run that saw the gate shut fails", not all(g["passed"] for g in grade_as_configured([activated, shut], "web-research", True)))
    expect("open file + a run with no decision fails", not all(g["passed"] for g in grade_as_configured([undecided], "web-research", True)))
    expect("closed file + shut runs pass", all(g["passed"] for g in grade_as_configured([shut, undecided], "web-research", False)))
    expect("closed file + an activated run fails", not all(g["passed"] for g in grade_as_configured([activated], "web-research", False)))
    expect("closed file + a run decided under an open gate fails", not all(g["passed"] for g in grade_as_configured([below], "web-research", False)))
    expect("a run that did not finish fails", not all(g["passed"] for g in grade_as_configured([{"verdict": None, "error": "x"}], "web-research", True)))

    # The A/B's activation gate follows the same rule: an open lane decides
    # every on run under the lane; it activates only what scale qualifies.
    off_run = {"verdict": "partial_pass", "activated": False, "gate_open": False, "decided_lane": "web-research",
               "working_set_searches": 0, "input_tokens": 90_000, "cost_usd": 0.31}
    on_below = {"verdict": "partial_pass", "activated": False, "gate_open": True, "decided_lane": "web-research",
                "decision_reason": "below every activation threshold (11264 of 24576 bytes beyond the window)",
                "narrowed_reads": 0, "working_set_searches": 0, "input_tokens": 90_000, "cost_usd": 0.31}
    names = {g["name"]: g["passed"] for g in grade([off_run], [on_below])}
    expect("A/B: an on run below the threshold is decided under the open lane", names.get("lane_open_when_open") is True)
    expect("A/B: the on arm may stay below the threshold", names.get("routing_closed_when_shut") is True and all(names.values()))
    on_shut = dict(on_below, gate_open=False)
    names = {g["name"]: g["passed"] for g in grade([off_run], [on_shut])}
    expect("A/B: an on run that saw the gate shut fails", names.get("lane_open_when_open") is False)
    return failures


def self_test_payload() -> dict[str, Any]:
    off = [{"verdict": "partial_pass", "activated": False, "gate_open": False, "decided_lane": "web-research",
            "working_set_searches": 0, "input_tokens": 90_000, "cost_usd": 0.31, "citations": 3, "content_reads": 6}]
    on = [{"verdict": "partial_pass", "activated": True, "gate_open": True, "decided_lane": "web-research",
           "activation_reason": "40960 bytes beyond the window meet the 24576-byte threshold",
           "narrowed_reads": 2, "working_set_searches": 3, "input_tokens": 41_000, "cost_usd": 0.12,
           "citations": 3, "content_reads": 6}]
    gates = grade(off, on)
    return {"mode": "self-test", "case": DEFAULT_CASE, "lane": DEFAULT_LANE,
            "runs": {"off": off, "on": on}, "gates": gates,
            "passed": all(g["passed"] for g in gates)}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--api-base-url", default=os.environ.get("HARNESS_CONFORMANCE_API_BASE_URL", DEFAULT_BASE_URL))
    parser.add_argument("--case", default=DEFAULT_CASE)
    parser.add_argument("--lane", default=DEFAULT_LANE)
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--config-path", type=Path, default=default_config_path())
    parser.add_argument("--data-root", type=Path, default=default_data_root())
    parser.add_argument("--http-timeout-secs", type=float, default=120.0)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--as-configured", action="store_true",
                        help="no config writes: run the case against the file as the operator left it and "
                             "check the runtime routes the way the file says (the check to run after opening a lane)")
    args = parser.parse_args(argv)

    if args.self_test:
        surgery_failures = self_test_config_surgery()
        for failure in surgery_failures:
            print(f"FAIL config surgery: {failure}")
        if not surgery_failures:
            print("PASS config surgery: the gate is set and nothing else moves, from an open, a closed, or a bare file")
        payload = self_test_payload()
        write_report(args.output_dir, payload)
        for gate in payload["gates"]:
            print(f"{'PASS' if gate['passed'] else 'FAIL'} {gate['name']}: {gate['detail']}")
        return 0 if payload["passed"] and not surgery_failures else 1
    if args.runs < 1:
        parser.error("--runs must be >= 1")
    original = args.config_path.read_text(encoding="utf-8")
    if args.dry_run:
        print(f"would run `{args.case}` {args.runs}x lane-off and {args.runs}x lane-on;")
        for arm, patched in (("lane-off", config_with_lane_closed(original)), ("lane-on", config_with_lane(original, args.lane))):
            print(f"{arm} config (working_sets block):")
            for line in patched.split("\n"):
                if line.startswith("    activation:") or line.startswith("      enabled_lanes:"):
                    print("  +", line)
        print("the file is restored as found afterwards")
        return 0

    client = Client(args.api_base_url, timeout=args.http_timeout_secs)
    login(client)
    if args.as_configured:
        lane_open = args.lane in enabled_lanes_in(original)
        print(f"file {args.config_path}: `{args.lane}` is {'OPEN' if lane_open else 'SHUT'}; no config writes", flush=True)
        configured: list[dict[str, Any]] = []
        for i in range(1, args.runs + 1):
            print(f"[as configured] run {i}/{args.runs} …", flush=True)
            configured.append(run_child(args, "as-configured", i))
            last = configured[-1]
            print(f"[as configured] run {i}: verdict={last.get('verdict')} activated={last.get('activated')} "
                  f"lane={last.get('activation_lane') or last.get('decided_lane')} gate_open={last.get('gate_open')} "
                  f"beyond_window={last.get('beyond_window_bytes')} narrowed={last.get('narrowed_reads')} "
                  f"searches={last.get('working_set_searches')} tokens={last.get('input_tokens')} "
                  f"cost=${last.get('cost_usd')} reason={last.get('activation_reason') or last.get('decision_reason')}", flush=True)
        gates = grade_as_configured(configured, args.lane, lane_open)
        payload = {"mode": "as-configured", "case": args.case, "lane": args.lane, "lane_open": lane_open,
                   "runs": {"off": [] if lane_open else configured, "on": configured if lane_open else []},
                   "gates": gates, "interrupted": False, "passed": all(g["passed"] for g in gates),
                   "config_path": str(args.config_path)}
        write_report(args.output_dir, payload)
        for gate in gates:
            print(f"{'PASS' if gate['passed'] else 'FAIL'} {gate['name']}: {gate['detail']}")
        print("PASSED" if payload["passed"] else "FAILED", f"— report: {args.output_dir / 'report.html'}")
        return 0 if payload["passed"] else 1
    runs: dict[str, list[dict[str, Any]]] = {"off": [], "on": []}
    interrupted = False
    restore_error = None

    def on_interrupt(signum, frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGINT, on_interrupt)
    try:
        # Lane off first. The baseline is the gate shut, not the file as found:
        # once a lane is open for good the operator's file is the on arm.
        args.config_path.write_text(config_with_lane_closed(original), encoding="utf-8")
        reload_runtime_config(client)
        for i in range(1, args.runs + 1):
            print(f"[lane off] run {i}/{args.runs} …", flush=True)
            runs["off"].append(run_child(args, "off", i))
            last = runs["off"][-1]
            print(f"[lane off] run {i}: verdict={last.get('verdict')} activated={last.get('activated')} "
                  f"tokens={last.get('input_tokens')} cost=${last.get('cost_usd')}", flush=True)
        args.config_path.write_text(config_with_lane(original, args.lane), encoding="utf-8")
        reload_runtime_config(client)
        for i in range(1, args.runs + 1):
            print(f"[lane on ] run {i}/{args.runs} …", flush=True)
            runs["on"].append(run_child(args, "on", i))
            last = runs["on"][-1]
            print(f"[lane on ] run {i}: verdict={last.get('verdict')} activated={last.get('activated')} "
                  f"beyond_window={last.get('beyond_window_bytes')} narrowed={last.get('narrowed_reads')} "
                  f"searches={last.get('working_set_searches')} tokens={last.get('input_tokens')} "
                  f"cost=${last.get('cost_usd')} reason={last.get('activation_reason') or last.get('decision_reason')}", flush=True)
    except KeyboardInterrupt:
        interrupted = True
    finally:
        try:
            args.config_path.write_text(original, encoding="utf-8")
            reload_runtime_config(client)
        except Exception as error:  # noqa: BLE001 — the restore must always report
            restore_error = str(error)
    gates = grade(runs["off"], runs["on"])
    payload = {"mode": "live", "case": args.case, "lane": args.lane, "runs": runs, "gates": gates,
               "interrupted": interrupted, "passed": (not interrupted) and all(g["passed"] for g in gates),
               "config_path": str(args.config_path)}
    write_report(args.output_dir, payload)
    for gate in gates:
        print(f"{'PASS' if gate['passed'] else 'FAIL'} {gate['name']}: {gate['detail']}")
    print("PASSED" if payload["passed"] else "FAILED", f"— report: {args.output_dir / 'report.html'}")
    if restore_error:
        print(f"CONFIG RESTORE FAILED: {restore_error} — restore {args.config_path} by hand", file=sys.stderr)
        return 3
    return 0 if payload["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())

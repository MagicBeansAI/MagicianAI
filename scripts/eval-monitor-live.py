#!/usr/bin/env python3
"""LIVE run-quality eval for Recurring Monitors (the acceptance instrument for
docs/components/magician/recurring-monitors.md "Outstanding verification" #2).

Drives REAL monitor executions through a RUNNING magician server against
LOCAL fixture pages this script serves itself (stdlib http.server on an
ephemeral 127.0.0.1 port — the eval's own harness, deterministic and
offline). Scratch monitors are created over the v3 API, run via run-now,
and physically deleted afterwards.

Scripted lifecycle matrix (per repetition):
  pricing        v1 -> baseline; swap v2 (Pro $49->$59 + new Team tier) ->
                 changed with a finding reflecting the mutated fact;
                 rerun v2 -> unchanged + NO new update record (quiet-run rule)
  release_notes  v1 -> baseline; swap v2 (new 2.4.0 "offline export" entry) ->
                 changed; rerun -> unchanged + quiet
  status         v1 -> baseline; swap v2 (INC-2107 investigating->resolved) ->
                 changed; rerun -> unchanged + quiet
  auth           fixture route always answers 401 -> run1 baseline (first
                 accepted run is ALWAYS baseline — compare_runs finalization),
                 run2 degraded + access_problem present + possibly_removed==0

Wire contract verified against magician/src/magician_v2/api/monitors_api.rs
(2026-07-23) — every route/field used here exists exactly:
  POST   /api/magician/v3/monitors                    {title?, spec, schedule?, agent_id?}
                                                      -> 201 {task_id, monitor_revision}
  POST   /api/magician/v3/monitors/{id}/run           -> 202 {task, execution}
                                                      (execution.state.execution_id)
  GET    /api/magician/v3/monitors/{id}/runs?limit=   -> {items: [MonitorRunResultV1], ...}
                                                      (status/counts/findings/access_problem)
  GET    /api/magician/v3/monitors/{id}/updates?limit=-> {items: [MonitorUpdateDetailV1], ...}
  DELETE /api/magician/v3/monitors/{id}?remove_files=true
                                                      (physical scratch cleanup)
Scope rides the opaque bearer in `MAGICIAN_BEARER_TOKEN`; the principal and
workspace env values remain report labels. Fixture pages: scripts/fixtures/monitor_live/.

Exit gates (env-tunable; non-zero exit on failure):
  - artifact-emission / terminal-extraction success rate == 100%
    (every 202-accepted run appears in GET .../runs before the timeout);
  - classification correctness == 100% across the scripted matrix
    (statuses match, the changed run's finding stable_key/title/summary
    reflects the mutated fact, degraded carries access_problem with
    counts.possibly_removed == 0);
  - zero false Changed updates on unchanged reruns (rerun status is not
    `changed` AND the updates count stays stable — the quiet-run rule).

Env:
  MAGICIAN_BASE_URL                 (default http://127.0.0.1:3002)
  MAGICIAN_PRINCIPAL / MAGICIAN_WORKSPACE (default live-eval / monitoring)
  MONITOR_LIVE_EVAL_RUNS            matrix repetitions (default 1)
  MONITOR_LIVE_CASES                comma list (default pricing,release_notes,status,auth)
  MONITOR_LIVE_AGENT_ID             optional (server default: personal-assistant)
  MONITOR_LIVE_RUN_TIMEOUT_SECS     per-run poll deadline (default 600)
  MONITOR_LIVE_CLEANUP_TIMEOUT_SECS cancellation + physical-delete deadline
                                    after each case (default 120)
  MONITOR_LIVE_POLL_SECS            poll interval (default 5)
  MONITOR_LIVE_FIXTURE_HOST         host the SERVER fetches fixtures from
                                    (default 127.0.0.1; containers may need
                                    host.container.internal)
  MONITOR_LIVE_FIXTURE_PORT         fixture server port (default 0 = ephemeral)
  MONITOR_LIVE_MIN_EXTRACTION_RATE  gate (default 1.0)
  MONITOR_LIVE_MIN_CLASSIFICATION_RATE gate (default 1.0)
  MONITOR_LIVE_MAX_FALSE_CHANGED    gate (default 0)
  MONITOR_LIVE_REPORT_DIR           (default coverage/evals/monitor-live)

Reports: <report dir>/latest.{html,json} + a timestamped run-*.json copy.
"""

import html
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BASE = os.environ.get("MAGICIAN_BASE_URL", "http://127.0.0.1:3002").rstrip("/")
API = f"{BASE}/api/magician/v3"
PRINCIPAL = os.environ.get("MAGICIAN_PRINCIPAL", "live-eval")
WORKSPACE = os.environ.get("MAGICIAN_WORKSPACE", "monitoring")
BEARER_TOKEN = os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
SCOPE = {"Authorization": f"Bearer {BEARER_TOKEN}"} if BEARER_TOKEN else {}
RUNS = int(os.environ.get("MONITOR_LIVE_EVAL_RUNS") or "1")
CASES = [
    c.strip()
    for c in (
        os.environ.get("MONITOR_LIVE_CASES") or "pricing,release_notes,status,auth"
    ).split(",")
    if c.strip()
]
AGENT_ID = (os.environ.get("MONITOR_LIVE_AGENT_ID") or "").strip() or None
RUN_TIMEOUT = int(os.environ.get("MONITOR_LIVE_RUN_TIMEOUT_SECS") or "600")
CLEANUP_TIMEOUT = int(os.environ.get("MONITOR_LIVE_CLEANUP_TIMEOUT_SECS") or "120")
POLL_SECS = int(os.environ.get("MONITOR_LIVE_POLL_SECS") or "5")
FIXTURE_HOST = os.environ.get("MONITOR_LIVE_FIXTURE_HOST", "127.0.0.1")
FIXTURE_PORT = int(os.environ.get("MONITOR_LIVE_FIXTURE_PORT") or "0")
MIN_EXTRACTION = float(os.environ.get("MONITOR_LIVE_MIN_EXTRACTION_RATE") or "1.0")
MIN_CLASSIFICATION = float(os.environ.get("MONITOR_LIVE_MIN_CLASSIFICATION_RATE") or "1.0")
MAX_FALSE_CHANGED = int(os.environ.get("MONITOR_LIVE_MAX_FALSE_CHANGED") or "0")
REPORT_DIR = os.environ.get("MONITOR_LIVE_REPORT_DIR") or "coverage/evals/monitor-live"

FIXTURE_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "monitor_live")


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ── Local fixture server (the eval's own harness) ────────────────────────────
# Logical route -> current fixture file; the matrix mutates VARIANTS to swap
# v1 -> v2 between runs. /dashboard always answers 401 (the auth wall).

VARIANTS = {}
VARIANTS_LOCK = threading.Lock()


def set_variant(route, filename):
    with VARIANTS_LOCK:
        VARIANTS[route] = filename


class FixtureHandler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 — BaseHTTPRequestHandler contract
        path = self.path.split("?", 1)[0]
        if path == "/dashboard":
            body = b"<html><body><h1>401 Unauthorized</h1><p>Sign in to view the analytics dashboard.</p></body></html>"
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="orbita-dashboard"')
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        with VARIANTS_LOCK:
            filename = VARIANTS.get(path)
        if not filename:
            self.send_response(404)
            self.end_headers()
            return
        with open(os.path.join(FIXTURE_DIR, filename), "rb") as f:
            body = f.read()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):  # keep eval output clean
        pass


def start_fixture_server():
    server = ThreadingHTTPServer(("0.0.0.0", FIXTURE_PORT), FixtureHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, server.server_address[1]


# ── HTTP client (urllib, mirroring eval-thinking-map-live.py) ────────────────


def http(method, url, body=None, timeout=90):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    for k, v in SCOPE.items():
        req.add_header(k, v)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode() or "{}")


# ── Matrix definition ────────────────────────────────────────────────────────
# Each content case runs the full lifecycle baseline -> changed -> unchanged;
# `keywords` must appear (case-insensitive) in some finding's
# stable_key/title/summary of the CHANGED run — the mutated-fact check.

CASE_DEFS = {
    "pricing": {
        "route": "/pricing",
        "v1": "pricing_v1.html",
        "v2": "pricing_v2.html",
        "objective": (
            "Watch the Orbita Tools pricing page and report material price or plan-tier "
            "changes (plan prices, new or removed tiers, included seats)."
        ),
        "include_rules": ["pricing table", "plan tiers", "monthly prices"],
        "keywords": ["59", "team"],
    },
    "release_notes": {
        "route": "/release-notes",
        "v1": "release_notes_v1.html",
        "v2": "release_notes_v2.html",
        "objective": (
            "Watch the Orbita CLI release-notes page and report newly published release "
            "entries (new version numbers and their headline features)."
        ),
        "include_rules": ["release entries", "version numbers", "feature lists"],
        "keywords": ["2.4.0", "offline export", "doctor"],
    },
    "status": {
        "route": "/status",
        "v1": "status_v1.html",
        "v2": "status_v2.html",
        "objective": (
            "Watch the Orbita Cloud status page and report component status changes and "
            "incident lifecycle updates (opened, investigating, resolved)."
        ),
        "include_rules": ["component status", "incident history"],
        "keywords": ["resolved", "inc-2107", "operational"],
    },
    "auth": {
        "route": "/dashboard",
        "v1": None,  # the route itself answers 401; no file
        "v2": None,
        "objective": (
            "Watch the Orbita analytics dashboard and report changes to the weekly "
            "signup and conversion metrics."
        ),
        "include_rules": ["signup metrics", "conversion metrics"],
        "keywords": [],
    },
}


def build_spec(case_name, base_url):
    d = CASE_DEFS[case_name]
    return {
        "schema_version": 1,
        "objective": d["objective"],
        "query_seeds": [],
        "sources": {
            "urls": [f"{base_url}{d['route']}"],
            "domains": [],
            "authenticated_sources": [],
        },
        "include_rules": d["include_rules"],
        "exclude_rules": ["footer boilerplate", "navigation"],
        "match_mode": "balanced",
        "notification_policy": "material_changes",
        "notify_initial_baseline": False,
    }


def create_monitor(case_name, base_url, repeat):
    body = {
        "title": f"eval-monitor-live-{case_name}-r{repeat}",
        "spec": build_spec(case_name, base_url),
        "schedule": None,  # unscheduled by design — run-now drives every run
    }
    if AGENT_ID:
        body["agent_id"] = AGENT_ID
    created = http("POST", f"{API}/monitors", body)
    return created["task_id"]


def run_and_await(task_id, on_execution=None):
    """POST run-now, then poll GET .../runs until the run record for that
    execution id appears (terminal extraction succeeded) or the deadline
    passes. Returns (run_record_or_None, execution_id, latency_seconds)."""
    started = time.time()
    accepted = http("POST", f"{API}/monitors/{task_id}/run", {})
    execution_id = accepted["execution"]["state"]["execution_id"]
    if on_execution is not None:
        on_execution(execution_id)
    deadline = started + RUN_TIMEOUT
    while time.time() < deadline:
        page = http("GET", f"{API}/monitors/{task_id}/runs?limit=50")
        for item in page.get("items", []):
            if item.get("execution_id") == execution_id:
                return item, execution_id, round(time.time() - started, 1)
        time.sleep(POLL_SECS)
    return None, execution_id, round(time.time() - started, 1)


def updates_count(task_id):
    page = http("GET", f"{API}/monitors/{task_id}/updates?limit=200")
    return len(page.get("items", []))


def finding_keyword_hit(run, keywords):
    if not keywords:
        return True
    for finding in run.get("findings", []) or []:
        blob = " ".join(
            str(finding.get(k) or "")
            for k in ("stable_key", "title", "summary", "why_it_matters")
        ).lower()
        if any(k.lower() in blob for k in keywords):
            return True
    return False


def step_result(case, repeat, phase, expected_status):
    return {
        "case": case,
        "repeat": repeat,
        "phase": phase,
        "expected_status": expected_status,
        "status": None,
        "execution_id": None,
        "extracted": False,
        "classified": False,
        "keyword_hit": None,
        "false_changed": False,
        "updates_before": None,
        "updates_after": None,
        "latency_s": None,
        "error": None,
    }


def run_step(
    task_id,
    case,
    repeat,
    phase,
    expected_status,
    keywords=None,
    on_execution=None,
):
    r = step_result(case, repeat, phase, expected_status)
    try:
        if phase == "unchanged":
            r["updates_before"] = updates_count(task_id)
        run, execution_id, latency = run_and_await(task_id, on_execution=on_execution)
        r["execution_id"] = execution_id
        r["latency_s"] = latency
        if run is None:
            r["error"] = f"no run record for {execution_id} within {RUN_TIMEOUT}s"
            return r
        r["extracted"] = True
        r["status"] = run.get("status")
        ok = r["status"] == expected_status
        if phase == "changed":
            r["keyword_hit"] = finding_keyword_hit(run, keywords or [])
            ok = ok and r["keyword_hit"]
        if phase == "degraded":
            counts = run.get("counts") or {}
            ok = (
                ok
                and bool(run.get("access_problem"))
                and int(counts.get("possibly_removed") or 0) == 0
            )
        if phase == "unchanged":
            r["updates_after"] = updates_count(task_id)
            r["false_changed"] = (
                r["status"] == "changed" or r["updates_after"] != r["updates_before"]
            )
            ok = ok and not r["false_changed"]
        r["classified"] = ok
    except Exception as exc:  # noqa: BLE001 — report, don't crash the suite
        r["error"] = str(exc)[:300]
    return r


def cancel_and_physically_delete_monitor(task_id, execution_ids):
    """Cancel any timed-out execution and apply the product's physical-delete
    contract before another eval case starts. Returning an error (instead of
    silently swallowing it) prevents one broken case from leaking while the
    harness creates more user-visible task records."""
    for execution_id in dict.fromkeys(value for value in execution_ids if value):
        try:
            http("POST", f"{API}/executions/{execution_id}/cancel", {})
        except urllib.error.HTTPError as exc:
            if exc.code not in (404, 409, 410, 422):
                return f"failed to cancel {execution_id}: HTTP {exc.code}"
        except Exception as exc:  # noqa: BLE001 — cleanup must become report data
            return f"failed to cancel {execution_id}: {str(exc)[:240]}"

    deadline = time.time() + CLEANUP_TIMEOUT
    last_error = None
    while time.time() < deadline:
        try:
            http("DELETE", f"{API}/monitors/{task_id}?remove_files=true")
            return None
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return None
            last_error = f"HTTP {exc.code}"
        except Exception as exc:  # noqa: BLE001 — retry bounded cleanup
            last_error = str(exc)[:240]
        time.sleep(min(POLL_SECS, 5))
    return (
        f"physical cleanup for {task_id} did not complete within "
        f"{CLEANUP_TIMEOUT}s ({last_error or 'unknown error'})"
    )


def run_case(case, base_url, repeat, on_result):
    d = CASE_DEFS[case]
    results = []
    execution_ids = []
    task_id = create_monitor(case, base_url, repeat)

    def record(result):
        results.append(result)
        on_result(result)

    try:
        if case == "auth":
            record(
                run_step(
                    task_id,
                    case,
                    repeat,
                    "baseline",
                    "baseline",
                    on_execution=execution_ids.append,
                )
            )
            record(
                run_step(
                    task_id,
                    case,
                    repeat,
                    "degraded",
                    "degraded",
                    on_execution=execution_ids.append,
                )
            )
        else:
            set_variant(d["route"], d["v1"])
            record(
                run_step(
                    task_id,
                    case,
                    repeat,
                    "baseline",
                    "baseline",
                    on_execution=execution_ids.append,
                )
            )
            set_variant(d["route"], d["v2"])
            record(
                run_step(
                    task_id,
                    case,
                    repeat,
                    "changed",
                    "changed",
                    d["keywords"],
                    on_execution=execution_ids.append,
                )
            )
            record(
                run_step(
                    task_id,
                    case,
                    repeat,
                    "unchanged",
                    "unchanged",
                    on_execution=execution_ids.append,
                )
            )
    finally:
        terminal_execution_ids = {
            result.get("execution_id")
            for result in results
            if result.get("extracted") and result.get("execution_id")
        }
        cleanup_error = cancel_and_physically_delete_monitor(
            task_id,
            [value for value in execution_ids if value not in terminal_execution_ids],
        )
        if cleanup_error:
            if results:
                prior = results[-1].get("error")
                results[-1]["error"] = "; ".join(
                    value for value in (prior, cleanup_error) if value
                )
                on_result(results[-1], replace=True)
            raise RuntimeError(cleanup_error)
    return results


def build_summary(results, fixture_base, completed, run_error=None):
    total = len(results)
    extracted = sum(1 for r in results if r["extracted"])
    classified = sum(1 for r in results if r["classified"])
    false_changed = sum(1 for r in results if r["false_changed"])
    latencies = sorted(r["latency_s"] for r in results if r["latency_s"] is not None)
    p50 = latencies[len(latencies) // 2] if latencies else None
    p95 = latencies[max(0, int(len(latencies) * 0.95) - 1)] if latencies else None

    summary = {
        "eval": "monitor-live",
        "generated_at": now_iso(),
        "server": BASE,
        "fixture_base": fixture_base,
        "scope": {
            "principal": PRINCIPAL,
            "workspace": WORKSPACE,
        },
        "completed": completed,
        "run_error": run_error,
        "matrix_steps": total,
        "extraction_rate": round(extracted / total, 3) if total else 0,
        "classification_rate": round(classified / total, 3) if total else 0,
        "false_changed_updates": false_changed,
        "latency_s_p50": p50,
        "latency_s_p95": p95,
        "gates": {
            "min_extraction_rate": MIN_EXTRACTION,
            "min_classification_rate": MIN_CLASSIFICATION,
            "max_false_changed": MAX_FALSE_CHANGED,
        },
        "steps": results,
    }
    return summary


def _atomic_write(path, content):
    temporary = f"{path}.tmp-{os.getpid()}"
    with open(temporary, "w", encoding="utf-8") as handle:
        handle.write(content)
    os.replace(temporary, path)


def write_report(summary, timestamped=False, stamp=None):
    os.makedirs(REPORT_DIR, exist_ok=True)
    encoded = json.dumps(summary, indent=2)
    _atomic_write(os.path.join(REPORT_DIR, "latest.json"), encoded)
    if timestamped:
        stamp = stamp or datetime.now().strftime("%Y%m%d-%H%M%S")
        _atomic_write(os.path.join(REPORT_DIR, f"run-{stamp}.json"), encoded)

    results = summary["steps"]
    rows = "".join(
        f"<tr><td>{html.escape(str(r['case']))}#{r['repeat']}</td>"
        f"<td>{html.escape(str(r['phase']))}</td>"
        f"<td>{html.escape(str(r['expected_status']))}</td>"
        f"<td>{html.escape(str(r['status']))}</td>"
        f"<td>{'yes' if r['classified'] else 'NO'}</td>"
        f"<td>{r['latency_s']}</td>"
        f"<td>{html.escape(str(r['error'] or ''))}</td></tr>"
        for r in results
    )
    metrics = "".join(
        f"<tr><td>{html.escape(str(k))}</td>"
        f"<td>{html.escape(json.dumps(v))}</td></tr>"
        for k, v in summary.items()
        if k != "steps"
    )
    html_report = (
        "<html><head><title>Recurring Monitors — live eval</title><style>"
        "body{font-family:system-ui;margin:2rem}table{border-collapse:collapse;margin:1rem 0}"
        "td,th{border:1px solid #ccc;padding:4px 10px;font-size:13px;text-align:left}"
        "</style></head><body><h1>Recurring Monitors — live run-quality eval</h1>"
        f"<table>{metrics}</table><h2>Matrix steps</h2>"
        "<table><tr><th>case</th><th>phase</th><th>expected</th><th>status</th>"
        f"<th>ok</th><th>s</th><th>error</th></tr>{rows}</table></body></html>"
    )
    _atomic_write(os.path.join(REPORT_DIR, "latest.html"), html_report)


def main():
    server, port = start_fixture_server()
    fixture_base = f"http://{FIXTURE_HOST}:{port}"
    print(f"fixture server on {fixture_base} (docroot {FIXTURE_DIR})", flush=True)

    results = []
    completed = False
    run_error = None
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")

    def checkpoint(result, replace=False):
        if replace and results:
            results[-1] = result
        else:
            results.append(result)
        write_report(build_summary(results, fixture_base, False), timestamped=False)
        ok = "ok" if (result["classified"] and not result["error"]) else "FAIL"
        print(
            f"[{ok}] {result['case']}#{result['repeat']}/{result['phase']} "
            f"status={result['status']} expected={result['expected_status']} "
            f"{result['latency_s']}s {result['error'] or ''}",
            flush=True,
        )

    caught = None
    try:
        for repeat in range(RUNS):
            for case in CASES:
                if case not in CASE_DEFS:
                    print(f"unknown case {case!r} — skipped", file=sys.stderr)
                    continue
                run_case(case, fixture_base, repeat, checkpoint)
        completed = True
    except BaseException as exc:  # report interruptions and fatal harness errors
        caught = exc
        run_error = "interrupted" if isinstance(exc, KeyboardInterrupt) else str(exc)[:500]
    finally:
        server.shutdown()
        summary = build_summary(results, fixture_base, completed, run_error)
        write_report(summary, timestamped=True, stamp=stamp)

    print(json.dumps({k: v for k, v in summary.items() if k != "steps"}, indent=2))

    if caught is not None:
        if isinstance(caught, KeyboardInterrupt):
            print("monitor live eval interrupted; partial report was preserved", file=sys.stderr)
            return 130
        raise caught

    failures = []
    if summary["extraction_rate"] < MIN_EXTRACTION:
        failures.append(
            f"terminal-extraction rate {summary['extraction_rate']} < {MIN_EXTRACTION}"
        )
    if summary["classification_rate"] < MIN_CLASSIFICATION:
        failures.append(
            f"classification rate {summary['classification_rate']} < {MIN_CLASSIFICATION}"
        )
    if false_changed > MAX_FALSE_CHANGED:
        failures.append(
            f"{false_changed} false Changed update(s) on unchanged reruns "
            f"(max {MAX_FALSE_CHANGED})"
        )
    if failures:
        print("GATE FAILURES: " + "; ".join(failures), file=sys.stderr)
        return 1
    print("ALL GATES PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())

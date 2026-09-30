#!/usr/bin/env python3
"""Provider-free golden eval for the Recurring Monitors change ledger.

Phase 6 of docs/plans/2026-07-21-recurring-monitors-productization-design-
implementation.md (checklist item 4). The eval substance lives in the Rust
integration suite `magician/tests/monitor_golden_scenarios.rs`, which drives
the plan's golden scenario set (public page price change, authenticated
dashboard with auth_failed, release-notes new entry, date/status flip,
transient failure then recovery, unchanged page, cosmetic-only change —
fixtures in `magician/tests/fixtures/monitors/golden/`) through the PURE
backend semantics: validate -> compare_runs -> finalize -> notification
policy -> update/Changed projection.

This script is a thin runner: it invokes that cargo test, parses the result,
and writes a coverage report the way the other eval scripts do
(coverage/evals/monitor/latest.{html,json} + a timestamped copy). Exit code
is non-zero when the suite fails.

DETERMINISTIC / PROVIDER-FREE by design: no LLM, no network, no running
server. The LIVE run-quality lane (real monitor executions against real
pages through a running magician) needs a provider and a server and is
explicitly out of scope here — it rides the live-eval harness when a lane is
added.

Env:
  MONITOR_EVAL_REPORT_DIR  (default coverage/evals/monitor)
  CARGO_TARGET_DIR         honored if set (the Makefile exports the SSD dir)
"""

import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPORT_DIR = os.environ.get("MONITOR_EVAL_REPORT_DIR") or "coverage/evals/monitor"
GOLDEN_DIR = os.path.join(REPO_ROOT, "magician", "tests", "fixtures", "monitors", "golden")

REQUIRED_SCENARIOS = [
    "public_page_price_change",
    "auth_dashboard_auth_failed",
    "release_notes_new_entry",
    "date_status_flip",
    "transient_failure_then_recovery",
    "unchanged_page",
    "cosmetic_only_change",
]


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_scenarios():
    scenarios = []
    for name in sorted(os.listdir(GOLDEN_DIR)):
        if not name.endswith(".json"):
            continue
        with open(os.path.join(GOLDEN_DIR, name)) as f:
            data = json.load(f)
        scenarios.append(
            {
                "file": name,
                "name": data.get("name"),
                "description": data.get("description"),
                "steps": len(data.get("steps", [])),
            }
        )
    return scenarios


def run_suite():
    cmd = [
        "cargo",
        "test",
        "-p",
        "magician",
        "--test",
        "monitor_golden_scenarios",
        "--",
        "--nocapture",
    ]
    proc = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    output = proc.stdout or ""
    # Standard libtest summary line: "test result: ok. N passed; M failed; ..."
    passed = failed = 0
    for match in re.finditer(r"(\d+) passed; (\d+) failed", output):
        passed += int(match.group(1))
        failed += int(match.group(2))
    return proc.returncode, passed, failed, output


def main():
    scenarios = load_scenarios()
    missing = [
        required
        for required in REQUIRED_SCENARIOS
        if required not in {scenario["name"] for scenario in scenarios}
    ]
    returncode, passed, failed, output = run_suite()
    ok = returncode == 0 and failed == 0 and not missing

    stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    summary = {
        "eval": "monitor-change-ledger-golden",
        "mode": "provider-free (deterministic Rust semantics; live LLM lane out of scope)",
        "generated_at": now_iso(),
        "ok": ok,
        "cargo_exit_code": returncode,
        "tests_passed": passed,
        "tests_failed": failed,
        "required_scenarios": REQUIRED_SCENARIOS,
        "missing_scenarios": missing,
        "scenarios": scenarios,
        "suite": "magician/tests/monitor_golden_scenarios.rs",
        "fixtures": "magician/tests/fixtures/monitors/golden/",
        "log_tail": output[-4000:],
    }

    os.makedirs(REPORT_DIR, exist_ok=True)
    with open(os.path.join(REPORT_DIR, "latest.json"), "w") as f:
        json.dump(summary, f, indent=2)
    with open(os.path.join(REPORT_DIR, f"run-{stamp}.json"), "w") as f:
        json.dump(summary, f, indent=2)

    rows = "".join(
        f"<tr><td>{scenario['name']}</td><td>{scenario['steps']}</td>"
        f"<td>{scenario['description']}</td></tr>"
        for scenario in scenarios
    )
    verdict = "PASS" if ok else "FAIL"
    color = "#2e7d32" if ok else "#c62828"
    html = (
        "<html><head><title>Monitor change-ledger golden eval</title><style>"
        "body{font-family:system-ui;margin:2rem}table{border-collapse:collapse}"
        "td,th{border:1px solid #ccc;padding:6px 10px;text-align:left}"
        f"h2 span{{color:{color}}}"
        "</style></head><body>"
        f"<h2>Monitor change-ledger golden eval — <span>{verdict}</span></h2>"
        f"<p>{summary['mode']}</p>"
        f"<p>Generated {summary['generated_at']} · cargo exit {returncode} · "
        f"{passed} passed / {failed} failed"
        + (f" · MISSING: {', '.join(missing)}" if missing else "")
        + "</p>"
        "<table><tr><th>Scenario</th><th>Steps</th><th>Description</th></tr>"
        f"{rows}</table>"
        "<p>Suite: <code>magician/tests/monitor_golden_scenarios.rs</code> · "
        "Fixtures: <code>magician/tests/fixtures/monitors/golden/</code></p>"
        "</body></html>"
    )
    with open(os.path.join(REPORT_DIR, "latest.html"), "w") as f:
        f.write(html)

    print(json.dumps({k: v for k, v in summary.items() if k != "log_tail"}, indent=2))
    if not ok:
        sys.stderr.write(output[-4000:] + "\n")
        sys.exit(1)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Provider-free recall eval for tool_search and find_agents_for_capability."""

from __future__ import annotations

import argparse
import html
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
GENERATED_BY = "magician::capability_recall_eval"


def run_exporter(root: Path) -> dict[str, Any]:
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", "/Volumes/build/magician/builds")
    env.setdefault("TMPDIR", "/Volumes/build/magician/tmp")
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "magician",
            "--example",
            "capability_recall_eval",
        ],
        cwd=root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode not in (0, 1):
        raise RuntimeError(
            "capability recall export failed: "
            + (completed.stderr.strip() or completed.stdout.strip())
        )
    payload = completed.stdout.strip()
    if not payload:
        raise RuntimeError(
            "capability recall export produced no JSON: "
            + (completed.stderr.strip() or f"exit {completed.returncode}")
        )
    try:
        report = json.loads(payload)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"capability recall export returned invalid JSON: {error}") from error
    report["_exporter_exit"] = completed.returncode
    return report


def validate_report(report: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    if report.get("schema_version") != SCHEMA_VERSION:
        failures.append("unsupported schema version")
    if report.get("generated_by") != GENERATED_BY:
        failures.append("report did not come from the production Rust evaluator")
    cases = report.get("cases")
    if not isinstance(cases, list) or not cases:
        failures.append("expected a non-empty cases list")
        cases = []
    golden = [case for case in cases if case.get("golden") is True]
    if len(golden) < 20:
        failures.append(f"expected at least 20 golden cases, got {len(golden)}")
    golden_failed = [
        str(case.get("query") or "")
        for case in golden
        if case.get("passed") is not True
    ]
    if golden_failed:
        failures.append("golden misses: " + ", ".join(golden_failed[:12]))
    expected_gate = "FAIL" if golden_failed else "PASS"
    if report.get("gate") != expected_gate:
        failures.append(f"gate is {report.get('gate')!r}, expected {expected_gate}")
    return failures


def render_html(report: dict[str, Any], failures: list[str]) -> str:
    rows = []
    for case in report.get("cases") or []:
        passed = case.get("passed") is True
        if passed and case.get("golden") is not True:
            continue
        if passed and case.get("golden") is True:
            cls = "pass"
            state = "PASS"
        elif case.get("golden") is True:
            cls = "fail"
            state = "GOLDEN FAIL"
        else:
            cls = "fail"
            state = "AUTO MISS"
        rows.append(
            "<tr>"
            f"<td>{html.escape(str(case.get('kind') or ''))}</td>"
            f"<td><code>{html.escape(str(case.get('query') or ''))}</code></td>"
            f"<td class='{cls}'>{state}</td>"
            f"<td><code>{html.escape(', '.join(case.get('expected') or []))}</code></td>"
            f"<td><code>{html.escape(', '.join(case.get('hits') or []))}</code></td>"
            f"<td>{html.escape(str(case.get('note') or ''))}</td>"
            "</tr>"
        )
    failure_list = "".join(f"<li>{html.escape(item)}</li>" for item in failures)
    auto_total = int(report.get("auto_total") or 0)
    auto_passed = int(report.get("auto_passed") or 0)
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Capability recall eval</title>
<style>
:root {{ color-scheme: light dark; font-family: ui-sans-serif,system-ui,sans-serif; }}
body {{ max-width: 1180px; margin: 0 auto; padding: 2rem; background: Canvas; color: CanvasText; }}
.summary {{ border: 1px solid color-mix(in srgb, CanvasText 18%, transparent); border-radius: 14px; padding: 1rem 1.2rem; }}
.pass {{ color: #16834a; font-weight: 750; }} .fail {{ color: #c23b3b; font-weight: 750; }}
table {{ width: 100%; border-collapse: collapse; margin-top: 1.25rem; }}
th,td {{ text-align: left; vertical-align: top; padding: .7rem; border-bottom: 1px solid color-mix(in srgb, CanvasText 14%, transparent); }}
code {{ white-space: pre-wrap; overflow-wrap: anywhere; font-size: .78rem; }}
</style></head><body>
<h1>Tool and agent recall eval</h1>
<div class="summary"><strong class={'pass' if not failures else 'fail'}>{'PASS' if not failures else 'FAIL'}</strong>
 · golden {int(report.get('golden_passed') or 0)}/{int(report.get('golden_total') or 0)}
 · auto {auto_passed}/{auto_total}
 · agents {int(report.get('agent_count') or 0)}
 · tool leaves {int(report.get('tool_leaf_count') or 0)}</div>
<p>Golden rows gate the run. Auto misses are inventory findings (name/alias/tool-leaf recall).</p>
{f'<ul>{failure_list}</ul>' if failure_list else ''}
<table><thead><tr><th>Kind</th><th>Query</th><th>Gate</th><th>Expected</th><th>Hits</th><th>Note</th></tr></thead>
<tbody>{''.join(rows)}</tbody></table>
</body></html>"""


def write_report(root: Path, report: dict[str, Any], failures: list[str], output_dir: Path | None) -> tuple[Path, Path]:
    report_root = Path(
        os.environ.get(
            "CAPABILITY_RECALL_EVAL_REPORT_DIR",
            str(root / "coverage/evals/capability-recall"),
        )
    ).expanduser()
    if output_dir is None:
        stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
        output_dir = report_root / "results" / stamp
    output_dir.mkdir(parents=True, exist_ok=True)
    json_path = output_dir / "report.json"
    html_path = output_dir / "index.html"
    payload = dict(report)
    payload.pop("_exporter_exit", None)
    payload["validation_failures"] = failures
    json_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    html_path.write_text(render_html(report, failures), encoding="utf-8")
    report_root.mkdir(parents=True, exist_ok=True)
    shutil.copy2(json_path, report_root / "latest.json")
    shutil.copy2(html_path, report_root / "latest.html")
    return json_path, html_path


def self_test() -> None:
    cases = [
        {
            "kind": "agent",
            "query": "web research",
            "expected": ["web-researcher"],
            "hits": ["web-researcher"],
            "golden": True,
            "passed": True,
            "note": "golden",
        }
        for _ in range(20)
    ]
    good = {
        "schema_version": 1,
        "generated_by": GENERATED_BY,
        "gate": "PASS",
        "golden_passed": 20,
        "golden_total": 20,
        "auto_passed": 0,
        "auto_total": 0,
        "agent_count": 1,
        "tool_leaf_count": 1,
        "cases": cases,
    }
    assert validate_report(good) == []
    bad = dict(good)
    bad["cases"] = list(cases)
    bad["cases"][0] = dict(cases[0], passed=False, query="web_research")
    bad["gate"] = "PASS"
    failures = validate_report(bad)
    assert any("golden misses" in item for item in failures)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--output-dir", type=Path)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    root = Path(__file__).resolve().parents[1]
    report = run_exporter(root)
    failures = validate_report(report)
    json_path, html_path = write_report(root, report, failures, args.output_dir)
    print(f"wrote {json_path}")
    print(f"wrote {html_path}")
    auto_total = int(report.get("auto_total") or 0)
    auto_passed = int(report.get("auto_passed") or 0)
    print(
        f"golden {report.get('golden_passed')}/{report.get('golden_total')} "
        f"auto {auto_passed}/{auto_total} gate {report.get('gate')}"
    )
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())

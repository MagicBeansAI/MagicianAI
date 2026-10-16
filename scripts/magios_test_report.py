#!/usr/bin/env python3
"""Generate a self-contained HTML report from an Xcode result bundle."""

from __future__ import annotations

import argparse
import html
import json
import os
import subprocess
import sys
import tempfile
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from typing import Any


def run_json(command: list[str]) -> tuple[dict[str, Any] | None, str | None]:
    try:
        completed = subprocess.run(command, check=False, capture_output=True, text=True)
    except OSError as error:
        return None, str(error)
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout).strip()
        return None, detail or f"command exited with {completed.returncode}"
    try:
        return json.loads(completed.stdout), None
    except json.JSONDecodeError as error:
        return None, f"invalid JSON from {' '.join(command[:3])}: {error}"


def bundle_is_fresh(path: Path, run_start_epoch: float | None) -> bool:
    if not path.exists():
        return False
    if run_start_epoch is None:
        return True
    # HFS+/APFS timestamps and test doubles may round to whole seconds.
    return path.stat().st_mtime >= run_start_epoch - 1


def atomic_write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        prefix=f".{path.name}.",
        dir=path.parent,
        delete=False,
    ) as temporary:
        temporary.write(content)
        temporary_path = Path(temporary.name)
    os.replace(temporary_path, path)


def escape(value: Any) -> str:
    return html.escape(str(value if value is not None else ""), quote=True)


def percentage(value: Any) -> float:
    try:
        return max(0.0, min(100.0, float(value) * 100.0))
    except (TypeError, ValueError):
        return 0.0


def status_class(value: Any) -> str:
    status = str(value or "Unknown").lower()
    if status == "passed":
        return "passed"
    if status == "failed":
        return "failed"
    if status == "skipped":
        return "skipped"
    return "unknown"


def collect_tests(tree: dict[str, Any] | None) -> list[dict[str, Any]]:
    tests: list[dict[str, Any]] = []

    def walk(node: dict[str, Any], bundle: str = "Tests", suite: str = "Tests") -> None:
        node_type = str(node.get("nodeType") or "")
        name = str(node.get("name") or "")
        next_bundle = name if node_type.lower().endswith("test bundle") else bundle
        next_suite = name if node_type == "Test Suite" else suite
        if node_type == "Test Case":
            failures = [
                str(child.get("name"))
                for child in node.get("children", [])
                if child.get("nodeType") == "Failure Message" and child.get("name")
            ]
            tests.append({
                "bundle": bundle,
                "suite": suite,
                "name": name,
                "identifier": node.get("nodeIdentifier") or name,
                "result": node.get("result") or "Unknown",
                "duration": node.get("durationInSeconds") or 0,
                "failures": failures,
            })
        for child in node.get("children", []):
            if isinstance(child, dict):
                walk(child, next_bundle, next_suite)

    for root in (tree or {}).get("testNodes", []):
        if isinstance(root, dict):
            walk(root)
    return tests


def report_exit_status(
    xcode_status: int,
    extraction_failed: bool,
    summary: dict[str, Any] | None,
    tree: dict[str, Any] | None,
) -> int:
    if xcode_status != 0:
        return 0
    reported_failure = (
        (summary or {}).get("result") == "Failed"
        or any(test["result"] == "Failed" for test in collect_tests(tree))
    )
    return 2 if extraction_failed or reported_failure else 0


def duration_text(seconds: Any) -> str:
    try:
        value = float(seconds)
    except (TypeError, ValueError):
        return "—"
    if value < 1:
        return f"{value * 1000:.0f} ms"
    if value < 60:
        return f"{value:.2f} s"
    return f"{int(value // 60)}m {value % 60:.1f}s"


def coverage_bar(value: float) -> str:
    tone = "good" if value >= 80 else "warn" if value >= 50 else "bad"
    return (
        f'<div class="coverage-value"><span>{value:.2f}%</span>'
        f'<div class="bar"><i class="{tone}" style="width:{value:.2f}%"></i></div></div>'
    )


def render_test_suites(tests: list[dict[str, Any]]) -> str:
    grouped: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for test in tests:
        grouped[(str(test["bundle"]), str(test["suite"]))].append(test)
    blocks: list[str] = []
    for (bundle, suite), cases in sorted(grouped.items()):
        passed = sum(case["result"] == "Passed" for case in cases)
        failed = sum(case["result"] == "Failed" for case in cases)
        duration = sum(float(case["duration"] or 0) for case in cases)
        open_attr = " open" if failed else ""
        rows: list[str] = []
        for case in sorted(cases, key=lambda item: str(item["name"]).lower()):
            result = str(case["result"])
            failure = "<br>".join(escape(value) for value in case["failures"])
            failure_detail = f'<div class="failure">{failure}</div>' if failure else ""
            rows.append(
                f'<tr class="test-row" data-result="{status_class(result)}" '
                f'data-search="{escape(bundle + " " + suite + " " + str(case["name"]))}">'
                f'<td><span class="status {status_class(result)}">{escape(result)}</span></td>'
                f'<td><strong>{escape(case["name"])}</strong>{failure_detail}</td>'
                f'<td class="duration">{duration_text(case["duration"])}</td></tr>'
            )
        blocks.append(
            f'<details class="suite"{open_attr}><summary><span>{escape(bundle)} / {escape(suite)}</span>'
            f'<span class="suite-meta">{passed} passed · {failed} failed · {duration_text(duration)}</span></summary>'
            '<div class="table-wrap"><table><thead><tr><th>Result</th><th>Test</th><th>Duration</th></tr></thead>'
            f'<tbody>{"".join(rows)}</tbody></table></div></details>'
        )
    return "".join(blocks) or '<div class="empty">No individual test cases were available.</div>'


def render_coverage(coverage: dict[str, Any] | None) -> str:
    targets = (coverage or {}).get("targets", [])
    if not targets:
        return '<div class="empty">Coverage was not available for this run.</div>'
    blocks: list[str] = []
    for target in sorted(targets, key=lambda item: str(item.get("name", "")).lower()):
        target_name = str(target.get("name") or "Target")
        target_coverage = percentage(target.get("lineCoverage"))
        files = sorted(target.get("files", []), key=lambda item: percentage(item.get("lineCoverage")))
        rows: list[str] = []
        for source in files:
            value = percentage(source.get("lineCoverage"))
            path = str(source.get("path") or source.get("name") or "")
            functions = [
                function for function in source.get("functions", [])
                if int(function.get("executableLines") or 0) >= 2 and percentage(function.get("lineCoverage")) < 100
            ]
            functions.sort(key=lambda function: percentage(function.get("lineCoverage")))
            weak = "".join(
                f'<li><code>{escape(function.get("name"))}</code><span>{percentage(function.get("lineCoverage")):.2f}%</span></li>'
                for function in functions[:20]
            )
            detail = (
                f'<details class="functions"><summary>{len(functions)} under-covered function(s)</summary><ul>{weak}</ul></details>'
                if functions else ""
            )
            rows.append(
                f'<tr><td><strong>{escape(source.get("name"))}</strong><div class="path">{escape(path)}</div>{detail}</td>'
                f'<td>{coverage_bar(value)}</td><td class="lines">{int(source.get("coveredLines") or 0)} / '
                f'{int(source.get("executableLines") or 0)}</td></tr>'
            )
        blocks.append(
            f'<details class="coverage-target" open><summary><span>{escape(target_name)}</span>'
            f'<span class="target-coverage">{target_coverage:.2f}% · {len(files)} files</span></summary>'
            '<div class="table-wrap"><table><thead><tr><th>File</th><th>Line coverage</th><th>Lines</th></tr></thead>'
            f'<tbody>{"".join(rows)}</tbody></table></div></details>'
        )
    return "".join(blocks)


def render_report(
    summary: dict[str, Any] | None,
    tree: dict[str, Any] | None,
    coverage: dict[str, Any] | None,
    bundle_path: Path,
    xcode_status: int,
    warnings: list[str],
    run_id: str | None = None,
    bundle_is_current: bool = True,
) -> str:
    tests = collect_tests(tree)
    summary = summary or {}
    case_failed = any(test["result"] == "Failed" for test in tests)
    result = (
        "Failed"
        if xcode_status != 0 or case_failed
        else str(summary.get("result") or "Unknown")
    )
    total = int(summary.get("totalTestCount") or len(tests))
    passed = int(summary.get("passedTests") or sum(test["result"] == "Passed" for test in tests))
    failed = int(summary.get("failedTests") or sum(test["result"] == "Failed" for test in tests))
    skipped = int(summary.get("skippedTests") or sum(test["result"] == "Skipped" for test in tests))
    start = summary.get("startTime")
    finish = summary.get("finishTime")
    elapsed = float(finish - start) if isinstance(start, (int, float)) and isinstance(finish, (int, float)) else 0
    failures = summary.get("testFailures", [])
    failure_rows = "".join(
        f'<li><strong>{escape(item.get("testName") or item.get("testIdentifierString"))}</strong>'
        f'<span>{escape(item.get("failureText"))}</span></li>' for item in failures
    )
    warning_html = "".join(f"<li>{escape(warning)}</li>" for warning in warnings)
    generated = datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
    bundle_uri = (
        bundle_path.resolve().as_uri() if bundle_is_current and bundle_path.exists() else ""
    )
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Magican iOS test report · {escape(result)}</title>
<style>
:root{{--bg:#0b1020;--panel:#121a2d;--panel2:#182238;--text:#edf2ff;--muted:#98a6c5;--line:#263454;--accent:#ff7777;--green:#42d392;--red:#ff6b81;--yellow:#f6c85f;--blue:#73a7ff}}
*{{box-sizing:border-box}} body{{margin:0;background:linear-gradient(145deg,#090d19,#111a30 60%,#151022);color:var(--text);font:14px/1.5 Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}}
main{{max-width:1240px;margin:auto;padding:36px 24px 80px}} h1{{font-size:34px;margin:5px 0}} h2{{margin:34px 0 12px;font-size:22px}} .eyebrow{{color:var(--accent);text-transform:uppercase;letter-spacing:.14em;font-size:11px;font-weight:800}}
.meta,.path{{color:var(--muted)}} .path{{font:11px ui-monospace,SFMono-Regular,monospace;overflow-wrap:anywhere;margin-top:3px}} .hero{{display:flex;align-items:flex-end;justify-content:space-between;gap:20px}}
.result{{padding:8px 14px;border-radius:99px;font-weight:800;text-transform:uppercase;letter-spacing:.08em}} .result.passed,.status.passed{{background:#17392f;color:var(--green)}} .result.failed,.status.failed{{background:#431e2a;color:var(--red)}} .result.unknown,.status.unknown,.result.skipped,.status.skipped{{background:#3d3520;color:var(--yellow)}}
.cards{{display:grid;grid-template-columns:repeat(5,minmax(120px,1fr));gap:12px;margin:26px 0}} .card{{background:rgba(18,26,45,.9);border:1px solid var(--line);border-radius:15px;padding:16px}} .card strong{{display:block;font-size:27px}} .card span{{color:var(--muted);font-size:12px}}
.failures,.warnings{{border:1px solid #6d2b3d;background:#27131c;border-radius:14px;padding:14px 18px}} .failures li,.warnings li{{margin:7px 0}} .failures span{{display:block;color:#ffc2cc}}
.toolbar{{display:flex;gap:10px;margin-bottom:12px}} input,select{{background:var(--panel);color:var(--text);border:1px solid var(--line);border-radius:9px;padding:10px 12px}} input{{flex:1}}
details.suite,details.coverage-target{{background:rgba(18,26,45,.92);border:1px solid var(--line);border-radius:13px;margin:10px 0;overflow:hidden}} summary{{cursor:pointer;display:flex;justify-content:space-between;gap:20px;padding:14px 16px;font-weight:750}} .suite-meta,.target-coverage{{color:var(--muted);font-weight:500}}
.table-wrap{{overflow:auto;border-top:1px solid var(--line)}} table{{width:100%;border-collapse:collapse}} th,td{{padding:10px 14px;text-align:left;border-bottom:1px solid var(--line);vertical-align:top}} th{{color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.08em}} tr:last-child td{{border-bottom:0}} .status{{display:inline-block;padding:3px 8px;border-radius:99px;font-size:10px;font-weight:800;text-transform:uppercase}} .duration,.lines{{white-space:nowrap;color:var(--muted)}} .failure{{color:#ff9cad;margin-top:4px}}
.coverage-value{{min-width:190px}} .coverage-value>span{{font-variant-numeric:tabular-nums}} .bar{{height:6px;margin-top:6px;background:#27334c;border-radius:99px;overflow:hidden}} .bar i{{display:block;height:100%;border-radius:99px}} .bar .good{{background:var(--green)}} .bar .warn{{background:var(--yellow)}} .bar .bad{{background:var(--red)}}
.functions summary{{padding:6px 0;color:var(--muted);font-size:11px;justify-content:flex-start}} .functions ul{{list-style:none;margin:0;padding:0;max-width:720px}} .functions li{{display:flex;justify-content:space-between;gap:16px;color:var(--muted);padding:2px 0}} code{{color:#c9d8ff}} .empty{{background:var(--panel);border:1px solid var(--line);padding:18px;border-radius:12px;color:var(--muted)}}
.raw-link{{color:var(--blue)}} footer{{margin-top:38px;color:var(--muted);font-size:12px}} @media(max-width:760px){{.cards{{grid-template-columns:repeat(2,1fr)}}.hero{{align-items:flex-start;flex-direction:column}}main{{padding:24px 14px}}}}
</style></head><body><main>
<div class="hero"><div><div class="eyebrow">Magican · iOS verification</div><h1>Test & coverage report</h1><div class="meta">Generated {escape(generated)}{f' · run {escape(run_id)}' if run_id else ''} · xcodebuild exit {xcode_status}{f' · <a class="raw-link" href="{escape(bundle_uri)}">raw xcresult</a>' if bundle_uri else ''}</div></div><span class="result {status_class(result)}">{escape(result)}</span></div>
<section class="cards"><div class="card"><strong>{total}</strong><span>Total tests</span></div><div class="card"><strong>{passed}</strong><span>Passed</span></div><div class="card"><strong>{failed}</strong><span>Failed</span></div><div class="card"><strong>{skipped}</strong><span>Skipped</span></div><div class="card"><strong>{duration_text(elapsed)}</strong><span>Duration</span></div></section>
{f'<section><h2>Failures</h2><ul class="failures">{failure_rows}</ul></section>' if failures else ''}
{f'<section><h2>Report warnings</h2><ul class="warnings">{warning_html}</ul></section>' if warnings else ''}
<section><h2>Test suites</h2><div class="toolbar"><input id="test-search" type="search" placeholder="Filter tests by bundle, suite, or name"><select id="status-filter"><option value="all">All results</option><option value="passed">Passed</option><option value="failed">Failed</option><option value="skipped">Skipped</option></select></div>{render_test_suites(tests)}</section>
<section><h2>Coverage</h2>{render_coverage(coverage)}</section>
<footer>Generated by <code>scripts/magios_test_report.py</code>. The report is self-contained and safe to open directly from disk.</footer>
</main><script>
const search=document.querySelector('#test-search'),filter=document.querySelector('#status-filter');
function apply(){{const q=search.value.toLowerCase(),s=filter.value;document.querySelectorAll('.test-row').forEach(row=>{{row.hidden=!(row.dataset.search.toLowerCase().includes(q)&&(s==='all'||row.dataset.result===s));}});}}
search.addEventListener('input',apply);filter.addEventListener('change',apply);
</script></body></html>"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xcresult", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--xcode-status", required=True, type=int)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-start-epoch", required=True, type=float)
    parser.add_argument("--warning", action="append", default=[])
    args = parser.parse_args()

    warnings: list[str] = list(args.warning)
    summary = tree = coverage = None
    fresh_bundle = bundle_is_fresh(args.xcresult, args.run_start_epoch)
    extraction_failed = False
    if fresh_bundle:
        summary, error = run_json(["xcrun", "xcresulttool", "get", "test-results", "summary", "--compact", "--path", str(args.xcresult)])
        if error:
            warnings.append(f"Test summary unavailable: {error}")
            extraction_failed = True
        tree, error = run_json(["xcrun", "xcresulttool", "get", "test-results", "tests", "--compact", "--path", str(args.xcresult)])
        if error:
            warnings.append(f"Test details unavailable: {error}")
            extraction_failed = True
        coverage, error = run_json(["xcrun", "xccov", "view", "--report", "--json", str(args.xcresult)])
        if error:
            warnings.append(f"Coverage unavailable: {error}")
            extraction_failed = True
        if summary is not None and summary.get("result") not in {"Passed", "Failed"}:
            warnings.append("Current-run test summary did not contain a recognized result.")
            extraction_failed = True
        if tree is not None and not collect_tests(tree):
            warnings.append("Current-run result bundle contained no individual test cases.")
            extraction_failed = True
        if coverage is not None and not coverage.get("targets"):
            warnings.append("Current-run result bundle contained no coverage targets.")
            extraction_failed = True
    elif args.xcresult.exists():
        warnings.append(
            f"Refused stale Xcode result bundle older than this run: {args.xcresult}"
        )
        extraction_failed = True
    else:
        warnings.append(f"Xcode result bundle was not created: {args.xcresult}")
        extraction_failed = True

    report = render_report(
        summary,
        tree,
        coverage,
        args.xcresult,
        args.xcode_status,
        warnings,
        run_id=args.run_id,
        bundle_is_current=fresh_bundle,
    )
    atomic_write_text(args.output, report)
    uri = args.output.resolve().as_uri()
    print()
    print(f"📊 iOS test report: {uri}")
    if sys.stdout.isatty():
        print(f"\033]8;;{uri}\033\\Open iOS test report\033]8;;\033\\")
    # A failed Xcode run remains authoritative even if its bundle is absent or
    # partially readable. A nominally successful run is not reportable as green
    # unless every current-run test/coverage surface was extracted successfully.
    return report_exit_status(args.xcode_status, extraction_failed, summary, tree)


if __name__ == "__main__":
    raise SystemExit(main())

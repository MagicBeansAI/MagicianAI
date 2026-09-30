#!/usr/bin/env python3
"""Generate a self-contained frontend test dashboard from Vitest artifacts."""

from __future__ import annotations

import argparse
import html
import json
import sys
from datetime import datetime
from pathlib import Path
from typing import Any


def escape(value: Any) -> str:
    return html.escape(str(value if value is not None else ""), quote=True)


def load_json(path: Path | None, label: str) -> tuple[dict[str, Any] | None, str | None]:
    if not path:
        return None, None
    if not path.exists():
        return None, f"{label} was not created: {path}"
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return None, f"{label} could not be read: {error}"
    if not isinstance(value, dict):
        return None, f"{label} did not contain a JSON object"
    return value, None


def status_class(value: Any) -> str:
    status = str(value or "unknown").lower()
    if status in {"passed", "failed", "skipped", "pending", "todo"}:
        return status
    return "unknown"


def duration_text(milliseconds: Any) -> str:
    try:
        value = max(0.0, float(milliseconds))
    except (TypeError, ValueError):
        return "—"
    if value < 1000:
        return f"{value:.0f} ms"
    seconds = value / 1000
    if seconds < 60:
        return f"{seconds:.2f} s"
    return f"{int(seconds // 60)}m {seconds % 60:.1f}s"


def source_name(value: Any) -> str:
    path = str(value or "Unknown test file")
    marker = "/ui/unified-ui/"
    return path.split(marker, 1)[1] if marker in path else path


def coverage_metric(entry: dict[str, Any], name: str) -> dict[str, Any]:
    metric = entry.get(name)
    return metric if isinstance(metric, dict) else {}


def coverage_pct(entry: dict[str, Any], name: str) -> float:
    try:
        return max(0.0, min(100.0, float(coverage_metric(entry, name).get("pct", 0))))
    except (TypeError, ValueError):
        return 0.0


def coverage_bar(value: float) -> str:
    tone = "good" if value >= 80 else "warn" if value >= 50 else "bad"
    return (
        f'<div class="coverage-value"><span>{value:.2f}%</span>'
        f'<div class="bar"><i class="{tone}" style="width:{value:.2f}%"></i></div></div>'
    )


def render_tests(results: dict[str, Any] | None) -> tuple[str, int]:
    blocks: list[str] = []
    case_count = 0
    for test_file in (results or {}).get("testResults", []):
        if not isinstance(test_file, dict):
            continue
        cases = [case for case in test_file.get("assertionResults", []) if isinstance(case, dict)]
        case_count += len(cases)
        failed = sum(status_class(case.get("status")) == "failed" for case in cases)
        passed = sum(status_class(case.get("status")) == "passed" for case in cases)
        duration = sum(float(case.get("duration") or 0) for case in cases)
        rows: list[str] = []
        for case in cases:
            status = status_class(case.get("status"))
            title = case.get("fullName") or case.get("title") or "Unnamed test"
            failures = case.get("failureMessages") or []
            if not isinstance(failures, list):
                failures = [failures]
            failure_html = "".join(f"<pre>{escape(message)}</pre>" for message in failures if message)
            searchable = " ".join([
                source_name(test_file.get("name")),
                *[str(value) for value in case.get("ancestorTitles", [])],
                str(title),
            ])
            rows.append(
                f'<tr class="test-row" data-result="{status}" data-search="{escape(searchable)}">'
                f'<td><span class="status {status}">{escape(status)}</span></td>'
                f'<td><strong>{escape(title)}</strong>{failure_html}</td>'
                f'<td class="duration">{duration_text(case.get("duration"))}</td></tr>'
            )
        open_attr = " open" if failed else ""
        blocks.append(
            f'<details class="suite"{open_attr}><summary><span>{escape(source_name(test_file.get("name")))}</span>'
            f'<span class="suite-meta">{passed} passed · {failed} failed · {duration_text(duration)}</span></summary>'
            '<div class="table-wrap"><table><thead><tr><th>Result</th><th>Test</th><th>Duration</th></tr></thead>'
            f'<tbody>{"".join(rows)}</tbody></table></div></details>'
        )
    return "".join(blocks) or '<div class="empty">No individual test cases were available.</div>', case_count


def render_coverage(summary: dict[str, Any] | None, coverage_html: Path | None) -> str:
    total = (summary or {}).get("total")
    if not isinstance(total, dict):
        return '<div class="empty">Coverage was not available for this run.</div>'
    metric_cards = "".join(
        f'<div class="coverage-card"><span>{escape(name.title())}</span><strong>{coverage_pct(total, name):.2f}%</strong></div>'
        for name in ("lines", "statements", "functions", "branches")
    )
    rows: list[str] = []
    sources = [
        (path, value) for path, value in (summary or {}).items()
        if path != "total" and isinstance(value, dict)
    ]
    sources.sort(key=lambda item: (coverage_pct(item[1], "lines"), item[0].lower()))
    for path, value in sources:
        lines = coverage_metric(value, "lines")
        line_count = f'{int(lines.get("covered") or 0)} / {int(lines.get("total") or 0)}'
        rows.append(
            f'<tr><td><strong>{escape(source_name(path))}</strong><div class="path">{escape(path)}</div></td>'
            f'<td>{coverage_bar(coverage_pct(value, "lines"))}</td>'
            f'<td>{coverage_pct(value, "functions"):.2f}%</td>'
            f'<td>{coverage_pct(value, "branches"):.2f}%</td><td class="duration">{line_count}</td></tr>'
        )
    coverage_link = (
        f'<a class="button" href="{escape(coverage_html.resolve().as_uri())}">Open source coverage details</a>'
        if coverage_html and coverage_html.exists() else ""
    )
    return (
        f'<div class="coverage-head"><div class="coverage-cards">{metric_cards}</div>{coverage_link}</div>'
        '<div class="table-wrap coverage-table"><table><thead><tr><th>Source file</th><th>Lines</th>'
        '<th>Functions</th><th>Branches</th><th>Covered lines</th></tr></thead>'
        f'<tbody>{"".join(rows)}</tbody></table></div>'
    )


def render_report(
    results: dict[str, Any] | None,
    coverage: dict[str, Any] | None,
    results_path: Path,
    coverage_html: Path | None,
    test_status: int,
    warnings: list[str],
) -> str:
    results = results or {}
    failed_run = test_status != 0 or results.get("success") is False
    result = "Failed" if failed_run else "Passed"
    total = int(results.get("numTotalTests") or 0)
    passed = int(results.get("numPassedTests") or 0)
    failed = int(results.get("numFailedTests") or 0)
    skipped = int(results.get("numPendingTests") or 0) + int(results.get("numTodoTests") or 0)
    start = results.get("startTime")
    ends = [item.get("endTime") for item in results.get("testResults", []) if isinstance(item, dict)]
    ends = [value for value in ends if isinstance(value, (int, float))]
    elapsed = max(ends) - start if isinstance(start, (int, float)) and ends else 0
    suites_html, case_count = render_tests(results)
    warning_html = "".join(f"<li>{escape(warning)}</li>" for warning in warnings)
    generated = datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
    raw_link = (
        f' · <a class="raw-link" href="{escape(results_path.resolve().as_uri())}">raw Vitest JSON</a>'
        if results_path.exists() else ""
    )
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Magican frontend test report · {result}</title>
<style>
:root{{--bg:#0b1020;--panel:#121a2d;--text:#edf2ff;--muted:#98a6c5;--line:#263454;--accent:#ff7777;--green:#42d392;--red:#ff6b81;--yellow:#f6c85f;--blue:#73a7ff}}
*{{box-sizing:border-box}} body{{margin:0;background:linear-gradient(145deg,#090d19,#111a30 60%,#151022);color:var(--text);font:14px/1.5 Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}}
main{{max-width:1240px;margin:auto;padding:36px 24px 80px}} h1{{font-size:34px;margin:5px 0}} h2{{margin:34px 0 12px;font-size:22px}} .eyebrow{{color:var(--accent);text-transform:uppercase;letter-spacing:.14em;font-size:11px;font-weight:800}}
.meta,.path{{color:var(--muted)}} .path{{font:11px ui-monospace,SFMono-Regular,monospace;overflow-wrap:anywhere;margin-top:3px}} .hero{{display:flex;align-items:flex-end;justify-content:space-between;gap:20px}}
.result{{padding:8px 14px;border-radius:99px;font-weight:800;text-transform:uppercase;letter-spacing:.08em}} .result.passed,.status.passed{{background:#17392f;color:var(--green)}} .result.failed,.status.failed{{background:#431e2a;color:var(--red)}} .status.skipped,.status.pending,.status.todo,.status.unknown{{background:#3d3520;color:var(--yellow)}}
.cards{{display:grid;grid-template-columns:repeat(5,minmax(120px,1fr));gap:12px;margin:26px 0}} .card,.coverage-card{{background:rgba(18,26,45,.9);border:1px solid var(--line);border-radius:15px;padding:16px}} .card strong{{display:block;font-size:27px}} .card span,.coverage-card span{{color:var(--muted);font-size:12px}}
.warnings{{border:1px solid #6d2b3d;background:#27131c;border-radius:14px;padding:14px 18px}} .toolbar{{display:flex;gap:10px;margin-bottom:12px}} input,select{{background:var(--panel);color:var(--text);border:1px solid var(--line);border-radius:9px;padding:10px 12px}} input{{flex:1}}
details.suite{{background:rgba(18,26,45,.92);border:1px solid var(--line);border-radius:13px;margin:10px 0;overflow:hidden}} summary{{cursor:pointer;display:flex;justify-content:space-between;gap:20px;padding:14px 16px;font-weight:750}} .suite-meta{{color:var(--muted);font-weight:500}}
.table-wrap{{overflow:auto;border:1px solid var(--line);border-radius:12px}} details .table-wrap{{border:0;border-top:1px solid var(--line);border-radius:0}} table{{width:100%;border-collapse:collapse}} th,td{{padding:10px 14px;text-align:left;border-bottom:1px solid var(--line);vertical-align:top}} th{{color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.08em}} tr:last-child td{{border-bottom:0}} .status{{display:inline-block;padding:3px 8px;border-radius:99px;font-size:10px;font-weight:800;text-transform:uppercase}} .duration{{white-space:nowrap;color:var(--muted)}} pre{{white-space:pre-wrap;color:#ff9cad;font:11px/1.45 ui-monospace,SFMono-Regular,monospace}}
.coverage-head{{display:flex;align-items:center;justify-content:space-between;gap:18px;margin-bottom:12px}} .coverage-cards{{display:grid;grid-template-columns:repeat(4,minmax(100px,1fr));gap:10px;flex:1}} .coverage-card strong{{display:block;font-size:21px}} .button{{background:#24385f;color:#cfe0ff;text-decoration:none;padding:10px 14px;border-radius:9px;white-space:nowrap}} .coverage-table{{background:rgba(18,26,45,.92)}}
.coverage-value{{min-width:160px}} .bar{{height:6px;margin-top:6px;background:#27334c;border-radius:99px;overflow:hidden}} .bar i{{display:block;height:100%}} .bar .good{{background:var(--green)}} .bar .warn{{background:var(--yellow)}} .bar .bad{{background:var(--red)}} .raw-link{{color:var(--blue)}} .empty{{background:var(--panel);border:1px solid var(--line);padding:18px;border-radius:12px;color:var(--muted)}} footer{{margin-top:38px;color:var(--muted);font-size:12px}}
@media(max-width:760px){{.cards{{grid-template-columns:repeat(2,1fr)}}.coverage-cards{{grid-template-columns:repeat(2,1fr)}}.coverage-head,.hero{{align-items:flex-start;flex-direction:column}}main{{padding:24px 14px}}}}
</style></head><body><main>
<div class="hero"><div><div class="eyebrow">Magican · frontend verification</div><h1>Test & coverage report</h1><div class="meta">Generated {escape(generated)} · test command exit {test_status} · {case_count} detailed cases{raw_link}</div></div><span class="result {result.lower()}">{result}</span></div>
<section class="cards"><div class="card"><strong>{total}</strong><span>Total tests</span></div><div class="card"><strong>{passed}</strong><span>Passed</span></div><div class="card"><strong>{failed}</strong><span>Failed</span></div><div class="card"><strong>{skipped}</strong><span>Skipped / todo</span></div><div class="card"><strong>{duration_text(elapsed)}</strong><span>Duration</span></div></section>
{f'<section><h2>Report warnings</h2><ul class="warnings">{warning_html}</ul></section>' if warnings else ''}
<section><h2>Test suites</h2><div class="toolbar"><input id="test-search" type="search" placeholder="Filter tests by file, suite, or name"><select id="status-filter"><option value="all">All results</option><option value="passed">Passed</option><option value="failed">Failed</option><option value="skipped">Skipped</option><option value="pending">Pending</option><option value="todo">Todo</option></select></div>{suites_html}</section>
<section><h2>Coverage</h2>{render_coverage(coverage, coverage_html)}</section>
<footer>Generated by <code>scripts/frontend_test_report.py</code>. The dashboard is self-contained; detailed source coverage remains local.</footer>
</main><script>
const search=document.querySelector('#test-search'),filter=document.querySelector('#status-filter');
function apply(){{const q=search.value.toLowerCase(),s=filter.value;document.querySelectorAll('.test-row').forEach(row=>{{row.hidden=!(row.dataset.search.toLowerCase().includes(q)&&(s==='all'||row.dataset.result===s));}});}}
search.addEventListener('input',apply);filter.addEventListener('change',apply);
</script></body></html>"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", required=True, type=Path)
    parser.add_argument("--coverage-summary", type=Path)
    parser.add_argument("--coverage-html", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--test-status", type=int, default=0)
    args = parser.parse_args()

    warnings: list[str] = []
    results, error = load_json(args.results, "Vitest results")
    if error:
        warnings.append(error)
    coverage, error = load_json(args.coverage_summary, "Coverage summary")
    if error:
        warnings.append(error)
    if args.coverage_html and not args.coverage_html.exists():
        warnings.append(f"Detailed coverage HTML was not created: {args.coverage_html}")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        render_report(results, coverage, args.results, args.coverage_html, args.test_status, warnings),
        encoding="utf-8",
    )
    uri = args.output.resolve().as_uri()
    print()
    print(f"📊 Frontend test report: {uri}")
    if sys.stdout.isatty():
        print(f"\033]8;;{uri}\033\\Open frontend test report\033]8;;\033\\")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

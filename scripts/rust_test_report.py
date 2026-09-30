#!/usr/bin/env python3
"""Generate a self-contained Rust test dashboard from nextest and LLVM artifacts."""

from __future__ import annotations

import argparse
import html
import json
import re
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any


@dataclass
class TestCase:
    suite: str
    name: str
    status: str
    duration: float = 0.0
    message: str = ""
    output: str = ""
    count: int = 1


@dataclass
class TestSuite:
    name: str
    cases: list[TestCase] = field(default_factory=list)


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


def child_text(element: ET.Element, names: tuple[str, ...]) -> str:
    values: list[str] = []
    for name in names:
        for child in element.findall(name):
            message = child.get("message") or ""
            body = child.text or ""
            text = "\n".join(part for part in (message.strip(), body.strip()) if part)
            if text:
                values.append(text)
    return "\n\n".join(values)


def parse_junit(path: Path) -> tuple[list[TestSuite], str | None]:
    if not path.exists():
        return [], f"nextest JUnit results were not created: {path}"
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError) as error:
        return [], f"nextest JUnit results could not be read: {error}"

    suite_elements = [root] if root.tag == "testsuite" else list(root.iter("testsuite"))
    suites: list[TestSuite] = []
    for suite_element in suite_elements:
        cases: list[TestCase] = []
        suite_name = suite_element.get("name") or "Rust tests"
        for case_element in suite_element.findall("testcase"):
            failure = child_text(case_element, ("failure", "error", "flakyFailure"))
            skipped = case_element.find("skipped") is not None
            status = "failed" if failure else "skipped" if skipped else "passed"
            try:
                duration = max(0.0, float(case_element.get("time") or 0))
            except ValueError:
                duration = 0.0
            output = child_text(case_element, ("system-out", "system-err"))
            cases.append(TestCase(
                suite=case_element.get("classname") or suite_name,
                name=case_element.get("name") or "Unnamed test",
                status=status,
                duration=duration,
                message=failure,
                output=output,
            ))
        if cases:
            suites.append(TestSuite(suite_name, cases))
    return suites, None


DOCTEST_CASE_RE = re.compile(r"^test (?P<name>.+) \.\.\. (?P<status>ok|FAILED|ignored)$")
DOCTEST_SUITE_RE = re.compile(r"^\s*Doc-tests (?P<name>\S+)")
NEXTEST_SKIPPED_RE = re.compile(r"\b\d+\s+tests run:.*?\b(?P<count>\d+)\s+skipped\b")


def parse_nextest_skipped(path: Path) -> tuple[list[TestSuite], str | None]:
    if not path.exists():
        return [], f"nextest terminal log was not created: {path}"
    try:
        value = path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        return [], f"nextest terminal log could not be read: {error}"
    match = NEXTEST_SKIPPED_RE.search(value)
    if not match:
        return [], None
    count = int(match.group("count"))
    if count == 0:
        return [], None
    suite_name = "nextest · ignored tests"
    return [TestSuite(suite_name, [TestCase(
        suite_name,
        f"{count} ignored tests were not executed by the default Cargo semantics",
        "skipped",
        count=count,
    )])], None


def parse_doctests(path: Path) -> tuple[list[TestSuite], str | None]:
    if not path.exists():
        return [], f"doctest log was not created: {path}"
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as error:
        return [], f"doctest log could not be read: {error}"

    suites: list[TestSuite] = []
    current: TestSuite | None = None
    for line in lines:
        suite_match = DOCTEST_SUITE_RE.match(line)
        if suite_match:
            current = TestSuite(f"doctest · {suite_match.group('name')}")
            suites.append(current)
            continue
        case_match = DOCTEST_CASE_RE.match(line)
        if not case_match:
            continue
        if current is None:
            current = TestSuite("doctests")
            suites.append(current)
        raw_status = case_match.group("status")
        status = "passed" if raw_status == "ok" else "skipped" if raw_status == "ignored" else "failed"
        current.cases.append(TestCase(current.name, case_match.group("name"), status))
    return [suite for suite in suites if suite.cases], None


def duration_text(seconds: Any) -> str:
    try:
        value = max(0.0, float(seconds))
    except (TypeError, ValueError):
        return "—"
    if value < 1:
        return f"{value * 1000:.0f} ms"
    if value < 60:
        return f"{value:.2f} s"
    return f"{int(value // 60)}m {value % 60:.1f}s"


def relative_source(value: Any) -> str:
    path = str(value or "Unknown source")
    marker = "/magician/"
    return path.split(marker, 1)[1] if marker in path else path


def metric(entry: dict[str, Any], name: str) -> dict[str, Any]:
    value = entry.get(name)
    return value if isinstance(value, dict) else {}


def metric_percent(entry: dict[str, Any], name: str) -> float:
    try:
        return max(0.0, min(100.0, float(metric(entry, name).get("percent", 0))))
    except (TypeError, ValueError):
        return 0.0


def metric_text(entry: dict[str, Any], name: str) -> str:
    value = metric(entry, name)
    try:
        count = int(value.get("count") or 0)
    except (TypeError, ValueError):
        count = 0
    return f"{metric_percent(entry, name):.2f}%" if count else "N/A"


def coverage_data(summary: dict[str, Any] | None) -> dict[str, Any]:
    data = (summary or {}).get("data")
    if not isinstance(data, list) or not data or not isinstance(data[0], dict):
        return {}
    return data[0]


def coverage_bar(value: float) -> str:
    tone = "good" if value >= 80 else "warn" if value >= 50 else "bad"
    return (
        f'<div class="coverage-value"><span>{value:.2f}%</span>'
        f'<div class="bar"><i class="{tone}" style="width:{value:.2f}%"></i></div></div>'
    )


def render_suites(suites: list[TestSuite]) -> str:
    blocks: list[str] = []
    for suite in suites:
        passed = sum(case.count for case in suite.cases if case.status == "passed")
        failed = sum(case.count for case in suite.cases if case.status == "failed")
        skipped = sum(case.count for case in suite.cases if case.status == "skipped")
        duration = sum(case.duration for case in suite.cases)
        rows: list[str] = []
        for case in suite.cases:
            diagnostic = ""
            if case.message:
                diagnostic += f'<pre class="failure">{escape(case.message)}</pre>'
            if case.output:
                diagnostic += f'<details class="output"><summary>Captured output</summary><pre>{escape(case.output)}</pre></details>'
            searchable = " ".join((suite.name, case.suite, case.name, case.status))
            rows.append(
                f'<tr class="test-row" data-result="{case.status}" data-search="{escape(searchable)}">'
                f'<td><span class="status {case.status}">{case.status}</span></td>'
                f'<td><strong>{escape(case.name)}</strong><div class="path">{escape(case.suite)}</div>{diagnostic}</td>'
                f'<td class="duration">{duration_text(case.duration)}</td></tr>'
            )
        meta = f"{passed} passed · {failed} failed · {skipped} skipped · {duration_text(duration)}"
        blocks.append(
            f'<details class="suite"{" open" if failed else ""}><summary><span>{escape(suite.name)}</span>'
            f'<span class="suite-meta">{meta}</span></summary><div class="table-wrap"><table><thead><tr>'
            f'<th>Result</th><th>Test</th><th>Duration</th></tr></thead><tbody>{"".join(rows)}</tbody></table></div></details>'
        )
    return "".join(blocks) or '<div class="empty">No individual Rust test cases were available.</div>'


def render_coverage(summary: dict[str, Any] | None, coverage_html: Path | None) -> str:
    data = coverage_data(summary)
    totals = data.get("totals")
    if not isinstance(totals, dict):
        return '<div class="empty">Coverage was not available for this run.</div>'

    cards = "".join(
        f'<div class="coverage-card"><span>{escape(label)}</span><strong>{metric_text(totals, name)}</strong></div>'
        for name, label in (("lines", "Lines"), ("functions", "Functions"), ("branches", "Branches"), ("regions", "Regions"))
    )
    files = [value for value in data.get("files", []) if isinstance(value, dict)]
    files.sort(key=lambda value: (metric_percent(value.get("summary", {}), "lines"), str(value.get("filename", "")).lower()))
    rows: list[str] = []
    for value in files:
        filename = value.get("filename") or "Unknown source"
        file_summary = value.get("summary") if isinstance(value.get("summary"), dict) else {}
        lines = metric(file_summary, "lines")
        rows.append(
            f'<tr><td><strong>{escape(relative_source(filename))}</strong><div class="path">{escape(filename)}</div></td>'
            f'<td>{coverage_bar(metric_percent(file_summary, "lines"))}</td>'
            f'<td>{metric_text(file_summary, "functions")}</td>'
            f'<td>{metric_text(file_summary, "branches")}</td>'
            f'<td class="duration">{int(lines.get("covered") or 0)} / {int(lines.get("count") or 0)}</td></tr>'
        )
    link = (
        f'<a class="button" href="{escape(coverage_html.resolve().as_uri())}">Open source coverage details</a>'
        if coverage_html and coverage_html.exists() else ""
    )
    return (
        f'<div class="coverage-head"><div class="coverage-cards">{cards}</div>{link}</div>'
        '<div class="coverage-note">Coverage instruments unit and integration tests. Stable Rust does not expose doctest coverage or '
        'branch regions; doctest pass/fail results are still included above, and unavailable branch metrics appear as N/A.</div>'
        '<div class="table-wrap coverage-table"><table><thead><tr><th>Source file</th><th>Lines</th>'
        '<th>Functions</th><th>Branches</th><th>Covered lines</th></tr></thead>'
        f'<tbody>{"".join(rows)}</tbody></table></div>'
    )


def render_report(
    suites: list[TestSuite],
    coverage: dict[str, Any] | None,
    junit_path: Path,
    nextest_log_path: Path,
    doctest_path: Path,
    coverage_json_path: Path,
    coverage_html: Path,
    nextest_status: int,
    doctest_status: int,
    coverage_status: int,
    warnings: list[str],
) -> str:
    cases = [case for suite in suites for case in suite.cases]
    total = sum(case.count for case in cases)
    passed = sum(case.count for case in cases if case.status == "passed")
    failed = sum(case.count for case in cases if case.status == "failed")
    skipped = sum(case.count for case in cases if case.status == "skipped")
    command_failed = nextest_status != 0 or doctest_status != 0 or coverage_status != 0
    result = "Failed" if command_failed or failed else "Passed"
    duration = sum(case.duration for case in cases)
    generated = datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
    warning_html = "".join(f"<li>{escape(warning)}</li>" for warning in warnings)
    raw_links: list[str] = []
    for label, path in (("JUnit XML", junit_path), ("nextest log", nextest_log_path), ("doctest log", doctest_path), ("coverage JSON", coverage_json_path)):
        if path and path.exists():
            raw_links.append(f'<a class="raw-link" href="{escape(path.resolve().as_uri())}">{escape(label)}</a>')
    artifacts = f" · {' · '.join(raw_links)}" if raw_links else ""

    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Magican Rust test report · {result}</title>
<style>
:root{{--bg:#08111a;--panel:#101e2a;--text:#edf7ff;--muted:#91a9bb;--line:#233b4d;--accent:#ff8a66;--green:#48d597;--red:#ff6f83;--yellow:#f5ca67;--blue:#76b9ff}}
*{{box-sizing:border-box}} body{{margin:0;background:radial-gradient(circle at 85% 0,#253248 0,transparent 34%),linear-gradient(145deg,#071018,#0d1c29 62%,#17142a);color:var(--text);font:14px/1.5 Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}}
main{{max-width:1280px;margin:auto;padding:36px 24px 80px}} h1{{font-size:34px;margin:5px 0}} h2{{margin:34px 0 12px;font-size:22px}} .eyebrow{{color:var(--accent);text-transform:uppercase;letter-spacing:.14em;font-size:11px;font-weight:800}}
.meta,.path,.coverage-note{{color:var(--muted)}} .path{{font:11px ui-monospace,SFMono-Regular,monospace;overflow-wrap:anywhere;margin-top:3px}} .hero{{display:flex;align-items:flex-end;justify-content:space-between;gap:20px}}
.result{{padding:8px 14px;border-radius:99px;font-weight:800;text-transform:uppercase;letter-spacing:.08em}} .result.passed,.status.passed{{background:#173b31;color:var(--green)}} .result.failed,.status.failed{{background:#461f2b;color:var(--red)}} .status.skipped{{background:#403821;color:var(--yellow)}}
.cards{{display:grid;grid-template-columns:repeat(5,minmax(120px,1fr));gap:12px;margin:26px 0}} .card,.coverage-card{{background:rgba(16,30,42,.92);border:1px solid var(--line);border-radius:15px;padding:16px}} .card strong{{display:block;font-size:27px}} .card span,.coverage-card span{{color:var(--muted);font-size:12px}}
.warnings{{border:1px solid #713047;background:#2b151f;border-radius:14px;padding:14px 18px}} .toolbar{{display:flex;gap:10px;margin-bottom:12px}} input,select{{background:var(--panel);color:var(--text);border:1px solid var(--line);border-radius:9px;padding:10px 12px}} input{{flex:1}}
details.suite{{background:rgba(16,30,42,.94);border:1px solid var(--line);border-radius:13px;margin:10px 0;overflow:hidden}} details.suite>summary{{cursor:pointer;display:flex;justify-content:space-between;gap:20px;padding:14px 16px;font-weight:750}} .suite-meta{{color:var(--muted);font-weight:500}}
.table-wrap{{overflow:auto;border:1px solid var(--line);border-radius:12px}} details .table-wrap{{border:0;border-top:1px solid var(--line);border-radius:0}} table{{width:100%;border-collapse:collapse}} th,td{{padding:10px 14px;text-align:left;border-bottom:1px solid var(--line);vertical-align:top}} th{{color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.08em}} tr:last-child td{{border-bottom:0}} .status{{display:inline-block;padding:3px 8px;border-radius:99px;font-size:10px;font-weight:800;text-transform:uppercase}} .duration{{white-space:nowrap;color:var(--muted)}} pre{{white-space:pre-wrap;font:11px/1.45 ui-monospace,SFMono-Regular,monospace}} pre.failure{{color:#ff9cad}} details.output{{margin-top:9px;color:var(--muted)}} details.output summary{{cursor:pointer}}
.coverage-head{{display:flex;align-items:center;justify-content:space-between;gap:18px;margin-bottom:12px}} .coverage-cards{{display:grid;grid-template-columns:repeat(4,minmax(100px,1fr));gap:10px;flex:1}} .coverage-card strong{{display:block;font-size:21px}} .button{{background:#284464;color:#d4e9ff;text-decoration:none;padding:10px 14px;border-radius:9px;white-space:nowrap}} .coverage-note{{margin:0 0 12px}} .coverage-table{{background:rgba(16,30,42,.94)}}
.coverage-value{{min-width:160px}} .bar{{height:6px;margin-top:6px;background:#263b4b;border-radius:99px;overflow:hidden}} .bar i{{display:block;height:100%}} .bar .good{{background:var(--green)}} .bar .warn{{background:var(--yellow)}} .bar .bad{{background:var(--red)}} .raw-link{{color:var(--blue)}} .empty{{background:var(--panel);border:1px solid var(--line);padding:18px;border-radius:12px;color:var(--muted)}} footer{{margin-top:38px;color:var(--muted);font-size:12px}}
@media(max-width:760px){{.cards{{grid-template-columns:repeat(2,1fr)}}.coverage-cards{{grid-template-columns:repeat(2,1fr)}}.coverage-head,.hero{{align-items:flex-start;flex-direction:column}}main{{padding:24px 14px}}}}
</style></head><body><main>
<div class="hero"><div><div class="eyebrow">Magican · Rust verification</div><h1>Test & coverage report</h1><div class="meta">Generated {escape(generated)} · nextest exit {nextest_status} · doctest exit {doctest_status} · coverage exit {coverage_status}{artifacts}</div></div><span class="result {result.lower()}">{result}</span></div>
<section class="cards"><div class="card"><strong>{total}</strong><span>Total tests</span></div><div class="card"><strong>{passed}</strong><span>Passed</span></div><div class="card"><strong>{failed}</strong><span>Failed</span></div><div class="card"><strong>{skipped}</strong><span>Skipped</span></div><div class="card"><strong>{duration_text(duration)}</strong><span>Recorded duration</span></div></section>
{f'<section><h2>Report warnings</h2><ul class="warnings">{warning_html}</ul></section>' if warnings else ''}
<section><h2>Test suites</h2><div class="toolbar"><input id="test-search" type="search" placeholder="Filter tests by crate, binary, module, or name"><select id="status-filter"><option value="all">All results</option><option value="passed">Passed</option><option value="failed">Failed</option><option value="skipped">Skipped</option></select></div>{render_suites(suites)}</section>
<section><h2>Coverage</h2>{render_coverage(coverage, coverage_html)}</section>
<footer>Generated by <code>scripts/rust_test_report.py</code>. The dashboard is self-contained; source-annotated coverage and raw artifacts remain local.</footer>
</main><script>
const search=document.querySelector('#test-search'),filter=document.querySelector('#status-filter');
function apply(){{const q=search.value.toLowerCase(),s=filter.value;document.querySelectorAll('.test-row').forEach(row=>{{row.hidden=!(row.dataset.search.toLowerCase().includes(q)&&(s==='all'||row.dataset.result===s));}});}}
search.addEventListener('input',apply);filter.addEventListener('change',apply);
</script></body></html>"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--junit", required=True, type=Path)
    parser.add_argument("--nextest-log", required=True, type=Path)
    parser.add_argument("--doctest-log", required=True, type=Path)
    parser.add_argument("--coverage-summary", type=Path)
    parser.add_argument("--coverage-html", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--nextest-status", type=int, default=0)
    parser.add_argument("--doctest-status", type=int, default=0)
    parser.add_argument("--coverage-status", type=int, default=0)
    args = parser.parse_args()

    warnings: list[str] = []
    suites, error = parse_junit(args.junit)
    if error:
        warnings.append(error)
    skipped_suites, error = parse_nextest_skipped(args.nextest_log)
    if error:
        warnings.append(error)
    suites.extend(skipped_suites)
    doctest_suites, error = parse_doctests(args.doctest_log)
    if error:
        warnings.append(error)
    suites.extend(doctest_suites)
    coverage, error = load_json(args.coverage_summary, "LLVM coverage summary")
    if error:
        warnings.append(error)
    if args.coverage_html and not args.coverage_html.exists():
        warnings.append(f"Detailed coverage HTML was not created: {args.coverage_html}")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(render_report(
        suites, coverage, args.junit, args.nextest_log, args.doctest_log, args.coverage_summary,
        args.coverage_html, args.nextest_status, args.doctest_status,
        args.coverage_status, warnings,
    ), encoding="utf-8")
    uri = args.output.resolve().as_uri()
    print()
    print(f"📊 Rust test report: {uri}")
    if sys.stdout.isatty():
        print(f"\033]8;;{uri}\033\\Open Rust test report\033]8;;\033\\")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Generate the top-level HTML dashboard for a composed repository test run."""

from __future__ import annotations

import argparse
import html
import json
import sys
from dataclasses import asdict, dataclass
from datetime import datetime
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class SuiteResult:
    name: str
    state: str
    exit_code: int
    target: str
    description: str
    report_kind: str
    report_path: Path | None

    @property
    def report_available(self) -> bool:
        return self.report_path is not None and self.report_path.is_file()


def escape(value: Any) -> str:
    return html.escape(str(value if value is not None else ""), quote=True)


def duration_text(seconds: int) -> str:
    seconds = max(0, seconds)
    if seconds < 60:
        return f"{seconds}s"
    hours, remainder = divmod(seconds, 3600)
    minutes, remaining_seconds = divmod(remainder, 60)
    if hours:
        return f"{hours}h {minutes}m {remaining_seconds}s"
    return f"{minutes}m {remaining_seconds}s"


def normalized_state(state: str, exit_code: int) -> str:
    if exit_code != 0:
        return "failed"
    return state if state in {"passed", "skipped"} else "passed"


def parse_suite(values: list[str]) -> SuiteResult:
    name, state, raw_exit, target, description, report_kind, raw_report = values
    exit_code = int(raw_exit)
    report_path = Path(raw_report).resolve() if raw_report else None
    return SuiteResult(
        name=name,
        state=normalized_state(state, exit_code),
        exit_code=exit_code,
        target=target,
        description=description,
        report_kind=report_kind,
        report_path=report_path,
    )


def parse_suite_manifest(path: Path, name_prefix: str = "") -> list[SuiteResult]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    raw_suites = payload.get("suites")
    if not isinstance(raw_suites, list):
        raise ValueError(f"suite manifest has no suites array: {path}")

    suites = []
    for index, raw_suite in enumerate(raw_suites):
        if not isinstance(raw_suite, dict):
            raise ValueError(f"suite manifest entry {index} is not an object: {path}")
        try:
            raw_report = raw_suite.get("report_path")
            report_path = Path(raw_report).resolve() if raw_report else None
            exit_code = int(raw_suite["exit_code"])
            suites.append(SuiteResult(
                name=f"{name_prefix}{raw_suite['name']}",
                state=normalized_state(str(raw_suite["state"]), exit_code),
                exit_code=exit_code,
                target=str(raw_suite["target"]),
                description=str(raw_suite["description"]),
                report_kind=str(raw_suite["report_kind"]),
                report_path=report_path,
            ))
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError(
                f"invalid suite manifest entry {index} in {path}: {error}"
            ) from error
    return suites


def suite_card(suite: SuiteResult) -> str:
    labels = {"passed": "Passed", "failed": "Failed", "skipped": "Skipped"}
    icons = {"passed": "✓", "failed": "×", "skipped": "–"}
    if suite.report_available:
        report_detail = (
            f'<a class="report-button" href="{escape(suite.report_path.resolve().as_uri())}">'
            "Open child report <span aria-hidden=\"true\">→</span></a>"
        )
    elif suite.report_kind == "html":
        report_detail = (
            '<span class="report-note">A child HTML report was not produced for this run.</span>'
        )
    else:
        report_detail = (
            '<span class="report-note">Status is recorded here; this suite has no dedicated HTML report.</span>'
        )
    return f"""
<article class="suite-card {suite.state}">
  <div class="suite-icon" aria-hidden="true">{icons[suite.state]}</div>
  <div class="suite-body">
    <div class="suite-heading">
      <div><h2>{escape(suite.name)}</h2><p>{escape(suite.description)}</p></div>
      <span class="status {suite.state}">{labels[suite.state]}</span>
    </div>
    <div class="suite-meta"><code>make {escape(suite.target)}</code><span>Exit {suite.exit_code}</span></div>
    <div class="suite-action">{report_detail}</div>
  </div>
</article>"""


def render_report(
    suites: list[SuiteResult],
    *,
    mode: str,
    run_id: str,
    started_at: str,
    duration_seconds: int,
    manifest_path: Path,
) -> str:
    failed = sum(suite.state == "failed" for suite in suites)
    passed = sum(suite.state == "passed" for suite in suites)
    skipped = sum(suite.state == "skipped" for suite in suites)
    reports = sum(suite.report_available for suite in suites)
    overall = "Failed" if failed else "Passed"
    generated = datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
    cards = "".join(suite_card(suite) for suite in suites)
    manifest_link = (
        f'<a href="{escape(manifest_path.resolve().as_uri())}">run manifest</a>'
        if manifest_path.exists()
        else "run manifest"
    )
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Magician complete test summary · {overall}</title>
<style>
:root{{--bg:#08101f;--panel:#111b2d;--panel2:#16233a;--text:#f2f6ff;--muted:#9baac5;--line:#293a59;--accent:#ff7a78;--green:#4bd7a2;--red:#ff7189;--yellow:#f5c85b;--blue:#8cb5ff}}
*{{box-sizing:border-box}} body{{margin:0;color:var(--text);font:14px/1.5 Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:radial-gradient(circle at 15% -10%,#26335e 0,transparent 34%),radial-gradient(circle at 90% 4%,#401e3a 0,transparent 28%),linear-gradient(150deg,#080d18,#0e1728 60%,#111827);min-height:100vh}}
main{{width:min(1120px,calc(100% - 32px));margin:auto;padding:48px 0 84px}} a{{color:var(--blue)}} .hero{{display:grid;grid-template-columns:1fr auto;align-items:end;gap:24px;padding-bottom:28px;border-bottom:1px solid var(--line)}}
.eyebrow{{color:var(--accent);font-size:11px;font-weight:850;letter-spacing:.16em;text-transform:uppercase}} h1{{font-size:clamp(32px,5vw,52px);letter-spacing:-.045em;line-height:1.03;margin:8px 0 13px}} .subtitle{{color:var(--muted);font-size:15px;max-width:700px;margin:0}}
.overall{{min-width:132px;text-align:center;padding:11px 18px;border-radius:999px;font-size:12px;font-weight:850;letter-spacing:.09em;text-transform:uppercase}} .overall.passed{{color:var(--green);background:#143b31;border:1px solid #23634f}} .overall.failed{{color:var(--red);background:#401c29;border:1px solid #713047}}
.metrics{{display:grid;grid-template-columns:repeat(5,minmax(110px,1fr));gap:12px;margin:24px 0 34px}} .metric{{padding:17px 18px;background:rgba(17,27,45,.86);border:1px solid var(--line);border-radius:15px;box-shadow:0 18px 48px rgba(0,0,0,.14)}} .metric strong{{display:block;font-size:27px;line-height:1.05}} .metric span{{display:block;color:var(--muted);font-size:12px;margin-top:6px}}
.suite-list{{display:grid;gap:12px}} .suite-card{{display:grid;grid-template-columns:54px 1fr;gap:2px;background:rgba(17,27,45,.9);border:1px solid var(--line);border-radius:18px;overflow:hidden;box-shadow:0 18px 60px rgba(0,0,0,.15)}} .suite-card.failed{{border-color:#6f3044}} .suite-card.skipped{{border-color:#635329}}
.suite-icon{{display:grid;place-items:center;font-size:24px;font-weight:850;background:var(--panel2)}} .suite-card.passed .suite-icon{{color:var(--green)}} .suite-card.failed .suite-icon{{color:var(--red)}} .suite-card.skipped .suite-icon{{color:var(--yellow)}} .suite-body{{padding:18px 20px}}
.suite-heading{{display:flex;align-items:flex-start;justify-content:space-between;gap:16px}} h2{{font-size:19px;margin:0 0 2px}} .suite-heading p{{color:var(--muted);margin:0}} .status{{flex:none;padding:4px 10px;border-radius:999px;font-size:10px;font-weight:850;letter-spacing:.08em;text-transform:uppercase}} .status.passed{{color:var(--green);background:#15372f}} .status.failed{{color:var(--red);background:#411c29}} .status.skipped{{color:var(--yellow);background:#3b341f}}
.suite-meta{{display:flex;gap:14px;align-items:center;margin:13px 0;color:var(--muted);font-size:12px}} code{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:#cedbfa;background:#0b1425;border:1px solid #253653;border-radius:7px;padding:3px 7px}} .suite-action{{min-height:38px;display:flex;align-items:center}} .report-button{{display:inline-flex;gap:8px;align-items:center;color:#e1eaff;text-decoration:none;background:#263b64;border:1px solid #3a568c;border-radius:9px;padding:8px 12px;font-weight:750}} .report-button:hover{{background:#304a7b}} .report-note{{color:var(--muted);font-size:12px}}
footer{{color:var(--muted);margin-top:32px;font-size:12px}} footer code{{padding:1px 5px}} @media(max-width:720px){{main{{width:min(100% - 22px,1120px);padding-top:26px}}.hero{{grid-template-columns:1fr;align-items:start}}.overall{{justify-self:start}}.metrics{{grid-template-columns:repeat(2,1fr)}}.suite-card{{grid-template-columns:42px 1fr}}.suite-body{{padding:15px 14px}}.suite-heading{{align-items:flex-start;flex-direction:column}}}}
</style></head><body><main>
<header class="hero"><div><div class="eyebrow">Magician · repository verification</div><h1>Complete test summary</h1><p class="subtitle">One view across Rust, frontend, desktop, macOS, iOS, and optional live LLM evals. Open any available child dashboard for its detailed tests, failures, and coverage.</p></div><span class="overall {overall.lower()}">{overall}</span></header>
<section class="metrics" aria-label="Run totals"><div class="metric"><strong>{len(suites)}</strong><span>Test suites</span></div><div class="metric"><strong>{passed}</strong><span>Passed</span></div><div class="metric"><strong>{failed}</strong><span>Failed</span></div><div class="metric"><strong>{skipped}</strong><span>Skipped</span></div><div class="metric"><strong>{reports}</strong><span>Child reports</span></div></section>
<section class="suite-list" aria-label="Suite results">{cards}</section>
<footer>Run <code>{escape(run_id)}</code> · {escape(mode)} mode · started {escape(started_at)} · duration {duration_text(duration_seconds)} · generated {escape(generated)} · {manifest_link}</footer>
</main></body></html>"""


def write_manifest(
    path: Path,
    suites: list[SuiteResult],
    *,
    mode: str,
    run_id: str,
    started_at: str,
    duration_seconds: int,
) -> None:
    serializable_suites = []
    for suite in suites:
        value = asdict(suite)
        value["report_path"] = str(suite.report_path) if suite.report_path else None
        value["report_available"] = suite.report_available
        serializable_suites.append(value)
    payload = {
        "run_id": run_id,
        "mode": mode,
        "started_at": started_at,
        "duration_seconds": max(0, duration_seconds),
        "overall": "failed" if any(suite.state == "failed" for suite in suites) else "passed",
        "suites": serializable_suites,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--archive-output", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--mode", choices=("standard", "verbose"), default="standard")
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--started-at", required=True)
    parser.add_argument("--duration-seconds", required=True, type=int)
    parser.add_argument(
        "--suite",
        action="append",
        nargs=7,
        required=True,
        metavar=("NAME", "STATE", "EXIT", "TARGET", "DESCRIPTION", "REPORT_KIND", "REPORT"),
    )
    parser.add_argument(
        "--suite-manifest",
        action="append",
        nargs=2,
        default=[],
        metavar=("NAME_PREFIX", "PATH"),
        help="Append child-suite entries from another generated test manifest.",
    )
    args = parser.parse_args()

    suites = [parse_suite(values) for values in args.suite]
    for name_prefix, raw_path in args.suite_manifest:
        suites.extend(parse_suite_manifest(Path(raw_path), name_prefix))
    write_manifest(
        args.manifest,
        suites,
        mode=args.mode,
        run_id=args.run_id,
        started_at=args.started_at,
        duration_seconds=args.duration_seconds,
    )
    report = render_report(
        suites,
        mode=args.mode,
        run_id=args.run_id,
        started_at=args.started_at,
        duration_seconds=args.duration_seconds,
        manifest_path=args.manifest,
    )
    for output in (args.output, args.archive_output):
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(report, encoding="utf-8")

    uri = args.output.resolve().as_uri()
    print()
    print(f"📊 Complete test summary: {uri}")
    if sys.stdout.isatty():
        print(f"\033]8;;{uri}\033\\Open complete test summary\033]8;;\033\\")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

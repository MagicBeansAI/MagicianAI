#!/usr/bin/env python3
"""Create a compact side-by-side report from two logical-chunk eval reports."""

from __future__ import annotations

import argparse
import html
import json
import os
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--baseline-label")
    parser.add_argument("--candidate-label")
    parser.add_argument("--title", default="Local model evaluation comparison")
    return parser.parse_args()


def load_report(path: Path) -> tuple[Path, dict[str, Any]]:
    resolved = path.resolve()
    if resolved.is_dir():
        resolved = resolved / "report.json"
    return resolved, json.loads(resolved.read_text(encoding="utf-8"))


def model_name(report: dict[str, Any]) -> str:
    return str(report.get("phase6", {}).get("local_model") or "unknown")


def case_map(report: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {str(case["operation"]): case for case in report.get("cases", [])}


def passed(case: dict[str, Any]) -> bool:
    return bool(
        case.get("success_rate") == 1.0
        and case.get("schema_validity_rate") == 1.0
        and case.get("golden_validity_rate") == 1.0
        and case.get("slo_pass") is True
    )


def latency(case: dict[str, Any]) -> int | None:
    value = case.get("latency_p50_ms")
    return int(value) if value is not None else None


def ratio_delta(baseline: int | None, candidate: int | None) -> float | None:
    if baseline in (None, 0) or candidate is None:
        return None
    return (candidate - baseline) / baseline


def make_summary(
    baseline_path: Path,
    baseline: dict[str, Any],
    candidate_path: Path,
    candidate: dict[str, Any],
    baseline_label: str | None = None,
    candidate_label: str | None = None,
    title: str = "Local model evaluation comparison",
) -> dict[str, Any]:
    baseline_cases = case_map(baseline)
    candidate_cases = case_map(candidate)
    operations = [
        operation for operation in baseline_cases if operation in candidate_cases
    ]
    rows = []
    for operation in operations:
        left = baseline_cases[operation]
        right = candidate_cases[operation]
        left_latency = latency(left)
        right_latency = latency(right)
        right_runs = right.get("runs", [])
        rows.append(
            {
                "operation": operation,
                "baseline_pass": passed(left),
                "candidate_pass": passed(right),
                "baseline_latency_ms": left_latency,
                "candidate_latency_ms": right_latency,
                "candidate_latency_delta_fraction": ratio_delta(
                    left_latency, right_latency
                ),
                "candidate_repairs": sum(
                    int(
                        (run.get("logical_chunking") or {}).get("local_repairs")
                        or 0
                    )
                    for run in right_runs
                ),
                "candidate_validation_errors": [
                    error
                    for run in right_runs
                    for error in run.get("validation_errors", [])
                ],
                "candidate_slo_failures": right.get("slo_failures", []),
            }
        )
    passing_rows = [row for row in rows if row["candidate_pass"]]
    baseline_passing_total = sum(
        int(row["baseline_latency_ms"] or 0) for row in passing_rows
    )
    candidate_passing_total = sum(
        int(row["candidate_latency_ms"] or 0) for row in passing_rows
    )
    return {
        "schema_version": "local_model_eval_comparison.v2",
        "title": title,
        "baseline": {
            "model": model_name(baseline),
            "label": baseline_label or model_name(baseline),
            "runtime": baseline.get("phase6", {}).get("local_runtime"),
            "report": str(baseline_path),
            "passed": sum(1 for row in rows if row["baseline_pass"]),
            "total": len(rows),
        },
        "candidate": {
            "model": model_name(candidate),
            "label": candidate_label or model_name(candidate),
            "runtime": candidate.get("phase6", {}).get("local_runtime"),
            "report": str(candidate_path),
            "passed": sum(1 for row in rows if row["candidate_pass"]),
            "total": len(rows),
        },
        "passing_case_latency_delta_fraction": ratio_delta(
            baseline_passing_total, candidate_passing_total
        ),
        "cases": rows,
    }


def percentage(value: float | None) -> str:
    if value is None:
        return "—"
    return f"{value * 100:+.1f}%"


def status(value: bool) -> str:
    return "PASS" if value else "FAIL"


def duration(value: int | None) -> str:
    return "—" if value is None else f"{value:,} ms"


def runtime_note(side: dict[str, Any]) -> str:
    runtime = side.get("runtime")
    if not isinstance(runtime, dict):
        return "runtime metadata not supplied"
    pieces = [
        str(runtime.get("kind") or "unknown runtime"),
        str(runtime.get("structured_output_mode") or "unspecified constraints"),
        (
            f"{runtime['context_tokens']} context tokens"
            if runtime.get("context_tokens") is not None
            else str(runtime.get("context_control") or "unspecified context")
        ),
        str(runtime.get("timing_source") or "unspecified timing"),
    ]
    return " · ".join(pieces)


def render(summary: dict[str, Any], output_dir: Path) -> str:
    baseline = summary["baseline"]
    candidate = summary["candidate"]
    rows = []
    for case in summary["cases"]:
        failures = case["candidate_validation_errors"] + case["candidate_slo_failures"]
        rows.append(
            "<tr>"
            f"<td>{html.escape(case['operation'])}</td>"
            f"<td class={'pass' if case['baseline_pass'] else 'fail'}>{status(case['baseline_pass'])}</td>"
            f"<td>{duration(case['baseline_latency_ms'])}</td>"
            f"<td class={'pass' if case['candidate_pass'] else 'fail'}>{status(case['candidate_pass'])}</td>"
            f"<td>{duration(case['candidate_latency_ms'])}</td>"
            f"<td>{percentage(case['candidate_latency_delta_fraction'])}</td>"
            f"<td>{case['candidate_repairs']}</td>"
            f"<td>{html.escape('; '.join(failures) or '—')}</td>"
            "</tr>"
        )
    baseline_link = Path(baseline["report"]).parent / "report.html"
    candidate_link = Path(candidate["report"]).parent / "report.html"
    baseline_href = os.path.relpath(baseline_link, output_dir)
    candidate_href = os.path.relpath(candidate_link, output_dir)
    constraint_warning = ""
    baseline_mode = (baseline.get("runtime") or {}).get("structured_output_mode")
    candidate_mode = (candidate.get("runtime") or {}).get("structured_output_mode")
    if baseline_mode and candidate_mode and baseline_mode != candidate_mode:
        constraint_warning = (
            '<p class="warning"><strong>Constraint mismatch:</strong> the two '
            "runtimes do not enforce structured output identically. Treat schema "
            "validity as a measured capability difference, not model-only quality.</p>"
        )
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><title>{html.escape(summary['title'])}</title>
<style>
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;margin:32px;color:#17202a;background:#f7f8fa}}
h1{{margin-bottom:4px}} .sub{{color:#5d6d7e;margin-top:0}}
.cards{{display:flex;gap:16px;flex-wrap:wrap;margin:24px 0}} .card{{background:white;border:1px solid #dfe4ea;border-radius:12px;padding:16px;min-width:190px}}
.card strong{{font-size:24px}} table{{width:100%;border-collapse:collapse;background:white;border:1px solid #dfe4ea}}
th,td{{text-align:left;padding:10px;border-bottom:1px solid #e8ebef;vertical-align:top}} th{{background:#eef2f6}} .pass{{color:#117a37;font-weight:700}} .fail{{color:#b42318;font-weight:700}}
code{{background:#eef2f6;padding:2px 5px;border-radius:4px}} a{{color:#175cd3}} .meta{{color:#5d6d7e;font-size:12px;margin-top:8px}} .warning{{background:#fff4e5;border:1px solid #f5c26b;border-radius:8px;padding:12px}}
</style></head><body>
<h1>{html.escape(summary['title'])}</h1>
<p class="sub">Identical Magician fixtures and quality gates. Runtime settings are taken from each source report rather than assumed.</p>
{constraint_warning}
<div class="cards">
<div class="card">{html.escape(baseline['label'])}<br><strong>{baseline['passed']}/{baseline['total']} PASS</strong><div class="meta">{html.escape(runtime_note(baseline))}</div></div>
<div class="card">{html.escape(candidate['label'])}<br><strong>{candidate['passed']}/{candidate['total']} PASS</strong><div class="meta">{html.escape(runtime_note(candidate))}</div></div>
<div class="card">Candidate latency on passing cases<br><strong>{percentage(summary['passing_case_latency_delta_fraction'])}</strong></div>
</div>
<table><thead><tr><th>Operation</th><th>{html.escape(baseline['label'])} gate</th><th>Baseline latency</th><th>{html.escape(candidate['label'])} gate</th><th>Candidate latency</th><th>Candidate delta</th><th>Repairs</th><th>Failure details</th></tr></thead>
<tbody>{''.join(rows)}</tbody></table>
<p><a href="{html.escape(str(baseline_href))}">{html.escape(baseline['label'])} detail</a> · <a href="{html.escape(str(candidate_href))}">{html.escape(candidate['label'])} detail</a> · <a href="comparison.json">Comparison JSON</a></p>
</body></html>"""


def main() -> int:
    args = parse_args()
    baseline_path, baseline = load_report(args.baseline)
    candidate_path, candidate = load_report(args.candidate)
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    summary = make_summary(
        baseline_path,
        baseline,
        candidate_path,
        candidate,
        baseline_label=args.baseline_label,
        candidate_label=args.candidate_label,
        title=args.title,
    )
    (output_dir / "comparison.json").write_text(
        json.dumps(summary, indent=2) + "\n", encoding="utf-8"
    )
    (output_dir / "report.html").write_text(
        render(summary, output_dir), encoding="utf-8"
    )
    print(f"Report: {(output_dir / 'report.html').as_uri()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Compare two local-runtime logical-chunk evals and native timing records."""

from __future__ import annotations

import argparse
import html
import json
import os
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    for side in ("left", "right"):
        parser.add_argument(f"--{side}-report", type=Path, required=True)
        parser.add_argument(f"--{side}-metrics", type=Path, required=True)
        parser.add_argument(f"--{side}-label", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--title", default="Local runtime comparison")
    return parser.parse_args()


def resolved_report(path: Path) -> Path:
    resolved = path.resolve()
    if resolved.is_dir():
        resolved = resolved / "report.json"
    return resolved


def read_json(path: Path) -> dict[str, Any]:
    resolved = resolved_report(path)
    value = json.loads(resolved.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise SystemExit(f"{resolved}: expected a JSON object")
    return value


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    return [
        json.loads(line)
        for line in path.resolve().read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def passed(case: dict[str, Any]) -> bool:
    return bool(
        case.get("success_rate") == 1.0
        and case.get("schema_validity_rate") == 1.0
        and case.get("golden_validity_rate") == 1.0
        and case.get("slo_pass") is True
    )


def physical_calls(case: dict[str, Any]) -> int:
    return sum(
        int((run.get("logical_chunking") or {}).get("physical_call_count") or 0)
        for run in case.get("runs", [])
    )


def aggregate(records: list[dict[str, Any]]) -> dict[str, Any]:
    prompt_tokens = sum(int(record.get("prompt_tokens") or 0) for record in records)
    prompt_ms = sum(float(record.get("prompt_duration_ms") or 0) for record in records)
    completion_tokens = sum(
        int(record.get("completion_tokens") or 0) for record in records
    )
    completion_throughput_tokens = sum(
        int(
            record.get("completion_throughput_tokens")
            if record.get("completion_throughput_tokens") is not None
            else record.get("completion_tokens")
            or 0
        )
        for record in records
    )
    completion_ms = sum(
        float(record.get("completion_duration_ms") or 0) for record in records
    )
    def values(key: str) -> list[str]:
        return sorted(
            {
                str(record[key])
                for record in records
                if record.get(key) not in (None, "")
            }
        )

    return {
        "request_count": len(records),
        "wall_duration_ms": round(
            sum(float(record.get("wall_duration_ms") or 0) for record in records), 3
        ),
        "native_duration_ms": round(
            sum(float(record.get("total_duration_ms") or 0) for record in records), 3
        ),
        "load_duration_ms": round(
            sum(float(record.get("load_duration_ms") or 0) for record in records), 3
        ),
        "prompt_tokens": prompt_tokens,
        "prompt_duration_ms": round(prompt_ms, 3),
        "prompt_tokens_per_second": (
            prompt_tokens / (prompt_ms / 1000) if prompt_ms > 0 else None
        ),
        "completion_tokens": completion_tokens,
        "completion_throughput_tokens": completion_throughput_tokens,
        "completion_duration_ms": round(completion_ms, 3),
        "completion_tokens_per_second": (
            completion_throughput_tokens / (completion_ms / 1000)
            if completion_ms > 0
            else None
        ),
        "backends": values("backend"),
        "timing_sources": values("timing_source"),
        "prompt_timing_semantics": values("prompt_timing_semantics"),
        "completion_timing_semantics": values("completion_timing_semantics"),
        "structured_output_modes": values("structured_output_mode"),
        "requested_context_tokens": values("requested_context_tokens"),
        "context_controls": values("context_control"),
        "model_lifecycles": values("model_lifecycle"),
        "reasoning_controls": values("reasoning_control"),
    }


def align(
    report: dict[str, Any], metrics: list[dict[str, Any]]
) -> dict[str, dict[str, Any]]:
    aligned: dict[str, dict[str, Any]] = {}
    cursor = 0
    for case in report.get("cases", []):
        count = physical_calls(case)
        records = metrics[cursor : cursor + count]
        if len(records) != count:
            raise SystemExit(
                f"{case.get('operation')}: expected {count} metrics, found {len(records)}"
            )
        cursor += count
        aligned[str(case["operation"])] = {
            "passed": passed(case),
            "latency_ms": case.get("latency_p50_ms"),
            "metrics": aggregate(records),
            "slo_failures": case.get("slo_failures", []),
        }
    if cursor != len(metrics):
        raise SystemExit(f"unused timing records: {len(metrics) - cursor}")
    return aligned


def delta(left: float | int | None, right: float | int | None) -> float | None:
    if left in (None, 0) or right is None:
        return None
    return (float(right) - float(left)) / float(left)


def build_summary(args: argparse.Namespace) -> dict[str, Any]:
    left_report = read_json(args.left_report)
    right_report = read_json(args.right_report)
    left_metrics = read_jsonl(args.left_metrics)
    right_metrics = read_jsonl(args.right_metrics)
    left_cases = align(left_report, left_metrics)
    right_cases = align(right_report, right_metrics)
    operations = [operation for operation in left_cases if operation in right_cases]
    rows = []
    for operation in operations:
        left = left_cases[operation]
        right = right_cases[operation]
        rows.append(
            {
                "operation": operation,
                "left": left,
                "right": right,
                "right_latency_delta_fraction": delta(
                    left["latency_ms"], right["latency_ms"]
                ),
            }
        )
    return {
        "schema_version": "local_runtime_eval_comparison.v2",
        "title": args.title,
        "left": {
            "label": args.left_label,
            "passed": sum(1 for row in rows if row["left"]["passed"]),
            "total": len(rows),
            "metrics": aggregate(left_metrics),
            "runtime": left_report.get("phase6", {}).get("local_runtime"),
            "report": str(resolved_report(args.left_report)),
        },
        "right": {
            "label": args.right_label,
            "passed": sum(1 for row in rows if row["right"]["passed"]),
            "total": len(rows),
            "metrics": aggregate(right_metrics),
            "runtime": right_report.get("phase6", {}).get("local_runtime"),
            "report": str(resolved_report(args.right_report)),
        },
        "cases": rows,
    }


def number(value: float | None, suffix: str = "") -> str:
    return "—" if value is None else f"{value:,.2f}{suffix}"


def percentage(value: float | None) -> str:
    return "—" if value is None else f"{value * 100:+.1f}%"


def joined(values: list[str], fallback: str = "unspecified") -> str:
    return ", ".join(values) if values else fallback


def report_href(report: str, output_dir: Path) -> str:
    target = Path(report).parent / "report.html"
    return os.path.relpath(target, output_dir)


def constraint_note(side: dict[str, Any]) -> str:
    metrics = side["metrics"]
    modes = joined(metrics["structured_output_modes"])
    sources = joined(metrics["timing_sources"])
    context = joined(metrics["context_controls"])
    reasoning = joined(metrics["reasoning_controls"])
    return (
        f"Structured output: {modes} · context: {context} · "
        f"reasoning: {reasoning} · timing: {sources}"
    )


def render(summary: dict[str, Any], output_dir: Path) -> str:
    left = summary["left"]
    right = summary["right"]
    rows = []
    for row in summary["cases"]:
        cells = []
        for side in ("left", "right"):
            result = row[side]
            metrics = result["metrics"]
            cells.extend(
                [
                    f"<td class={'pass' if result['passed'] else 'fail'}>{'PASS' if result['passed'] else 'FAIL'}</td>",
                    f"<td>{number(result['latency_ms'], ' ms')}</td>",
                    f"<td>{number(metrics['prompt_tokens_per_second'])}</td>",
                    f"<td>{number(metrics['completion_tokens_per_second'])}</td>",
                    f"<td>{metrics['prompt_tokens']:,} / {metrics['completion_tokens']:,}</td>",
                ]
            )
        rows.append(
            "<tr>"
            f"<td>{html.escape(row['operation'])}</td>"
            + "".join(cells[:5])
            + f"<td>{percentage(row['right_latency_delta_fraction'])}</td>"
            + "".join(cells[5:])
            + "</tr>"
        )
    left_href = report_href(left["report"], output_dir)
    right_href = report_href(right["report"], output_dir)
    constraint_warning = ""
    if (
        left["metrics"]["structured_output_modes"]
        != right["metrics"]["structured_output_modes"]
    ):
        constraint_warning = (
            '<p class="warning"><strong>Constraint mismatch:</strong> these lanes '
            "do not enforce structured output identically. Quality remains directly "
            "measured, but schema validity is partly a runtime-capability comparison.</p>"
        )
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><title>{html.escape(summary['title'])}</title>
<style>
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;margin:32px;color:#17202a;background:#f7f8fa}}
.cards{{display:flex;gap:16px;flex-wrap:wrap;margin:24px 0}}.card{{background:#fff;border:1px solid #dfe4ea;border-radius:12px;padding:16px;min-width:210px}}
.card strong{{font-size:22px}}.meta{{color:#5d6d7e;font-size:12px;margin-top:8px}}.warning{{background:#fff4e5;border:1px solid #f5c26b;border-radius:8px;padding:12px}}table{{width:100%;border-collapse:collapse;background:#fff;border:1px solid #dfe4ea;font-size:14px}}th,td{{padding:9px;border-bottom:1px solid #e8ebef;text-align:left;vertical-align:top}}th{{background:#eef2f6}}.pass{{color:#117a37;font-weight:700}}.fail{{color:#b42318;font-weight:700}}a{{color:#175cd3}}code{{background:#eef2f6;padding:2px 5px;border-radius:4px}}
</style></head><body>
<h1>{html.escape(summary['title'])}</h1>
<p>Same logical-chunk fixtures and quality gates, with runtime capabilities and timing semantics carried into the report.</p>
{constraint_warning}
<div class="cards">
<div class="card">{html.escape(left['label'])}<br><strong>{left['passed']}/{left['total']} PASS</strong><br>Prompt/TTFT-derived {number(left['metrics']['prompt_tokens_per_second'])} tok/s · generation {number(left['metrics']['completion_tokens_per_second'])} tok/s<div class="meta">{html.escape(constraint_note(left))}</div></div>
<div class="card">{html.escape(right['label'])}<br><strong>{right['passed']}/{right['total']} PASS</strong><br>Prompt/TTFT-derived {number(right['metrics']['prompt_tokens_per_second'])} tok/s · generation {number(right['metrics']['completion_tokens_per_second'])} tok/s<div class="meta">{html.escape(constraint_note(right))}</div></div>
</div>
<table><thead><tr><th>Operation</th><th>{html.escape(left['label'])} gate</th><th>Latency</th><th>Prompt/TTFT tok/s</th><th>Gen tok/s</th><th>Prompt / output tokens</th><th>{html.escape(right['label'])} latency delta</th><th>{html.escape(right['label'])} gate</th><th>Latency</th><th>Prompt/TTFT tok/s</th><th>Gen tok/s</th><th>Prompt / output tokens</th></tr></thead><tbody>{''.join(rows)}</tbody></table>
<p><strong>Interpretation:</strong> total latency is output-length dependent. Native Ollama/llama-server prompt and generation timing comes from the backend. For MLX-LM/OpenAI-chat, prompt throughput is derived from observed TTFT (which includes queue/load/prefill/first-token time) and generation throughput uses the remaining stream window; those are useful estimates, not native phase timings.</p>
<p><a href="{html.escape(str(left_href))}">{html.escape(left['label'])} details</a> · <a href="{html.escape(str(right_href))}">{html.escape(right['label'])} details</a> · <a href="comparison.json">Comparison JSON</a></p>
</body></html>"""


def main() -> int:
    args = parse_args()
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    summary = build_summary(args)
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

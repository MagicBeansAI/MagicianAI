#!/usr/bin/env python3
"""Read-only Phase 0 baseline for LLM call/dispatch population and volume."""

from __future__ import annotations

import argparse
import html
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


CALL_POPULATION_FIELDS = [
    "principal",
    "workspace",
    "execution_id",
    "task_id",
    "plan_id",
    "step_id",
    "agent_id",
    "delegated_agent_id",
    "chat_session_id",
    "operation",
    "profile",
    "provider",
    "model",
    "capability",
    "response_kind",
    "attempt",
    "success",
    "input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "cost_usd",
    "latency_ms",
    "ttft_ms",
]
REQUIRED_FUTURE_CALL_FIELDS = [
    "llm_call_id",
    "provider_attempt_id",
    "dispatch_job_id",
    "trace_id",
    "chat_turn_id",
    "iteration_id",
    "capture_status",
    "parse_success",
    "schema_validation_success",
    "contract_validation_success",
    "training_eligible_at_capture",
]


def sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def parquet_glob(root: Path) -> str:
    return str(root / "dt=*" / "*.parquet")


def discover_scopes(runtime_root: Path, principal: str | None, workspace: str | None) -> list[tuple[str, str, Path]]:
    scopes_root = runtime_root / "scopes"
    if principal and workspace:
        return [(principal, workspace, scopes_root / principal / workspace)]
    scopes: list[tuple[str, str, Path]] = []
    if not scopes_root.is_dir():
        return scopes
    for principal_dir in sorted(path for path in scopes_root.iterdir() if path.is_dir()):
        for workspace_dir in sorted(path for path in principal_dir.iterdir() if path.is_dir()):
            scopes.append((principal_dir.name, workspace_dir.name, workspace_dir))
    return scopes


def parquet_files(root: Path) -> list[Path]:
    return sorted(root.glob("dt=*/*.parquet")) if root.is_dir() else []


def describe_columns(connection: Any, glob: str) -> list[str]:
    rows = connection.execute(
        f"DESCRIBE SELECT * FROM read_parquet({sql_string(glob)}, union_by_name=true, hive_partitioning=true)"
    ).fetchall()
    return [str(row[0]) for row in rows]


def populated_count(connection: Any, view: str, column: str) -> int:
    query = (
        f'SELECT count(*) FILTER (WHERE "{column}" IS NOT NULL '
        f'AND length(trim(cast("{column}" AS VARCHAR))) > 0) FROM {view}'
    )
    return int(connection.execute(query).fetchone()[0])


def dataset_report(connection: Any, dataset_id: str, root: Path, population_fields: list[str]) -> dict[str, Any]:
    files = parquet_files(root)
    report: dict[str, Any] = {
        "dataset": dataset_id,
        "root": str(root),
        "file_count": len(files),
        "partition_count": len({path.parent.name for path in files}),
        "bytes": sum(path.stat().st_size for path in files),
        "rows": 0,
        "columns": [],
        "population": {},
        "top_operations": [],
        "error": None,
    }
    if not files:
        return report
    glob = parquet_glob(root)
    view = f"phase0_{dataset_id.replace('-', '_')}"
    try:
        connection.execute(
            f"CREATE OR REPLACE TEMP VIEW {view} AS SELECT * FROM read_parquet("
            f"{sql_string(glob)}, union_by_name=true, hive_partitioning=true)"
        )
        columns = describe_columns(connection, glob)
        report["columns"] = columns
        total = int(connection.execute(f"SELECT count(*) FROM {view}").fetchone()[0])
        report["rows"] = total
        for field in population_fields:
            if field not in columns:
                report["population"][field] = {"present": False, "count": 0, "rate": 0.0}
                continue
            count = populated_count(connection, view, field)
            report["population"][field] = {
                "present": True,
                "count": count,
                "rate": round(count / total, 6) if total else 0.0,
            }
        if "operation" in columns:
            report["top_operations"] = [
                {"operation": row[0] or "", "rows": int(row[1])}
                for row in connection.execute(
                    f"SELECT operation, count(*) AS n FROM {view} GROUP BY operation ORDER BY n DESC, operation LIMIT 20"
                ).fetchall()
            ]
        timestamp_column = "timestamp_ms" if "timestamp_ms" in columns else None
        if timestamp_column:
            minimum, maximum = connection.execute(
                f'SELECT min("{timestamp_column}"), max("{timestamp_column}") FROM {view}'
            ).fetchone()
            report["timestamp_min_ms"] = minimum
            report["timestamp_max_ms"] = maximum
    except Exception as error:  # report partial/corrupt historical partitions
        report["error"] = str(error)
    return report


def human_bytes(value: int) -> str:
    size = float(value)
    for suffix in ("B", "KiB", "MiB", "GiB", "TiB"):
        if size < 1024.0 or suffix == "TiB":
            return f"{size:.1f} {suffix}"
        size /= 1024.0
    return f"{value} B"


def render_html(report: dict[str, Any]) -> str:
    scope_rows = []
    for scope in report["scopes"]:
        call = scope["llm_calls"]
        dispatch = scope["llm_dispatch"]
        scope_rows.append(
            "<tr>"
            f"<td>{html.escape(scope['principal'])}/{html.escape(scope['workspace'])}</td>"
            f"<td>{call['rows']:,}</td><td>{call['file_count']}</td><td>{human_bytes(call['bytes'])}</td>"
            f"<td>{dispatch['rows']:,}</td><td>{dispatch['file_count']}</td><td>{human_bytes(dispatch['bytes'])}</td>"
            "</tr>"
        )
    population_rows = []
    for field, facts in report["aggregate_call_population"].items():
        population_rows.append(
            "<tr>"
            f"<td><code>{html.escape(field)}</code></td>"
            f"<td>{'yes' if facts['present'] else 'no'}</td>"
            f"<td>{facts['count']:,}</td><td>{facts['rate'] * 100:.2f}%</td>"
            "</tr>"
        )
    missing = "".join(f"<li><code>{html.escape(name)}</code></li>" for name in report["future_fields_absent"])
    errors = "".join(f"<li>{html.escape(error)}</li>" for error in report["errors"]) or "<li>None</li>"
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>LLM observability Phase 0 baseline</title>
<style>
body{{font-family:Inter,system-ui,sans-serif;background:#0b1020;color:#e8ecf7;margin:0;padding:32px}}main{{max-width:1180px;margin:auto}}
h1,h2{{letter-spacing:-.02em}}.meta,.card{{background:#121a2e;border:1px solid #273452;border-radius:14px;padding:18px;margin:16px 0}}
table{{width:100%;border-collapse:collapse}}th,td{{text-align:left;border-bottom:1px solid #273452;padding:9px}}th{{color:#9fb3d9}}code{{color:#9dd9ff}}
.status{{display:inline-block;padding:5px 10px;border-radius:999px;background:#183d32;color:#8af0c4}}.warn{{background:#4a3217;color:#ffd596}}
</style></head><body><main>
<h1>LLM observability Phase 0 baseline</h1>
<div class="meta"><span class="status{' warn' if report['status'] != 'passed' else ''}">{html.escape(report['status'])}</span>
<p>Generated {html.escape(report['generated_at'])}. Read-only, content-free scan of LLM call and dispatch Parquet facts.</p></div>
<section class="card"><h2>Scope and volume</h2><table><thead><tr><th>Scope</th><th>Call rows</th><th>Call files</th><th>Call bytes</th><th>Dispatch rows</th><th>Dispatch files</th><th>Dispatch bytes</th></tr></thead><tbody>{''.join(scope_rows)}</tbody></table></section>
<section class="card"><h2>Current call-field population</h2><table><thead><tr><th>Field</th><th>Column exists</th><th>Populated</th><th>Rate</th></tr></thead><tbody>{''.join(population_rows)}</tbody></table></section>
<section class="card"><h2>Fields required by later phases but absent today</h2><ul>{missing}</ul></section>
<section class="card"><h2>Read errors</h2><ul>{errors}</ul></section>
<section class="card"><h2>Interpretation</h2><p>Transport success is not task quality. This baseline measures schema population and storage volume only; it deliberately does not infer outcomes from call order or read prompt/response content.</p></section>
</main></body></html>"""


def aggregate_population(scopes: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    total_rows = sum(scope["llm_calls"]["rows"] for scope in scopes)
    result: dict[str, dict[str, Any]] = {}
    for field in CALL_POPULATION_FIELDS:
        present = any(scope["llm_calls"]["population"].get(field, {}).get("present", False) for scope in scopes)
        count = sum(scope["llm_calls"]["population"].get(field, {}).get("count", 0) for scope in scopes)
        result[field] = {
            "present": present,
            "count": count,
            "rate": round(count / total_rows, 6) if total_rows else 0.0,
        }
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime-root", type=Path, default=Path(os.environ.get("MAGICIAN_ROOT_DIR", "~/MagicianNotes")).expanduser())
    parser.add_argument("--principal")
    parser.add_argument("--workspace")
    parser.add_argument("--output-dir", type=Path, default=Path("coverage/evals/llm-observability-phase0/latest"))
    parser.add_argument("--strict", action="store_true", help="Fail when no llm_calls data exists or a partition cannot be read")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if bool(args.principal) != bool(args.workspace):
        parser.error("--principal and --workspace must be supplied together")
    if args.dry_run:
        print(json.dumps({"runtime_root": str(args.runtime_root), "output_dir": str(args.output_dir), "content_fields_read": []}, indent=2))
        return 0
    try:
        import duckdb  # type: ignore
    except ImportError:
        print("duckdb Python package is required for the Phase 0 baseline", file=sys.stderr)
        return 2

    connection = duckdb.connect()
    scopes: list[dict[str, Any]] = []
    errors: list[str] = []
    for principal, workspace, scope_root in discover_scopes(args.runtime_root, args.principal, args.workspace):
        call = dataset_report(connection, "llm_calls", scope_root / "analytics/llm_calls", CALL_POPULATION_FIELDS)
        dispatch = dataset_report(connection, "llm_dispatch", scope_root / "analytics/llm_dispatch", [])
        if call["error"]:
            errors.append(f"{principal}/{workspace} llm_calls: {call['error']}")
        if dispatch["error"]:
            errors.append(f"{principal}/{workspace} llm_dispatch: {dispatch['error']}")
        if call["file_count"] or dispatch["file_count"] or (args.principal and args.workspace):
            scopes.append({"principal": principal, "workspace": workspace, "llm_calls": call, "llm_dispatch": dispatch})
    aggregate = aggregate_population(scopes)
    existing_columns = {column for scope in scopes for column in scope["llm_calls"]["columns"]}
    total_calls = sum(scope["llm_calls"]["rows"] for scope in scopes)
    status = "passed" if total_calls and not errors else ("no_data" if not total_calls and not errors else "partial")
    report = {
        "schema_version": 1,
        "evaluation": "llm-observability-phase0-baseline",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "status": status,
        "runtime_root": str(args.runtime_root),
        "scopes": scopes,
        "aggregate_call_population": aggregate,
        "future_fields_absent": [field for field in REQUIRED_FUTURE_CALL_FIELDS if field not in existing_columns],
        "sink_contract": json.loads((Path(__file__).resolve().parents[1] / "data/magician_v2/llm_observability/sink-contract-v1.json").read_text(encoding="utf-8")),
        "privacy": {"content_fields_read": [], "raw_payloads_written": False, "report_contains_only_counts_schema_and_paths": True},
        "outcome_baseline": {
            "transport_success_available": "success" in existing_columns,
            "terminal_outcomes_in_call_facts": False,
            "interpretation": "Current success is transport/coarse call success. Delayed terminal outcomes require the Phase 4 append-only outcome join and must not be inferred from call ordering.",
        },
        "errors": errors,
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.output_dir / "report.json"
    html_path = args.output_dir / "report.html"
    json_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    html_path.write_text(render_html(report), encoding="utf-8")
    print(f"LLM observability Phase 0: {status}; calls={total_calls}; scopes={len(scopes)}")
    print(f"Report: {html_path.resolve().as_uri()}")
    if args.strict and status != "passed":
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

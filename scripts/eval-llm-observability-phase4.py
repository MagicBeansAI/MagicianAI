#!/usr/bin/env python3
"""Fact-only live audit for Phase 4 model-to-tool lineage.

The evaluator reads only canonical metadata Parquet. It never writes tool
arguments, results, delegated content, or stable IDs into its reports.
"""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from datetime import datetime, timezone
import glob
import html
import json
import os
from pathlib import Path
import re
import sys
import time
from typing import Any


SAFE_SCOPE = re.compile(r"^[^/\\:*?\"<>|\x00-\x1f]{1,255}$")
REQUIRED_STAGES = {
    "proposed",
    "name_validated",
    "arguments_parsed",
    "schema_validated",
    "authorization_resolved",
    "approval_resolved",
    "execution_started",
    "execution_finished",
    "result_validated",
    "result_consumed",
    "branch_materialized",
    "rollback_started",
    "rollback_finished",
    "linkage_gap",
}
OUTCOME_STAGES = {"execution_finished", "result_validated", "branch_materialized", "linkage_gap"}
MAX_AUDIT_ROWS = 250_000


def validate_scope(value: str, name: str) -> None:
    if value in {".", ".."} or value.strip() != value or not SAFE_SCOPE.fullmatch(value):
        raise ValueError(f"invalid {name} scope component")


def sql_list(files: list[str]) -> str:
    return "[" + ",".join("'" + path.replace("'", "''") + "'" for path in files) + "]"


def parquet_files(root: Path, principal: str, workspace: str, dataset: str) -> list[str]:
    candidates = [
        root / "magician_data_v3" / "scopes" / principal / workspace / "analytics" / dataset,
        root / "scopes" / principal / workspace / "analytics" / dataset,
    ]
    for candidate in candidates:
        files = sorted(glob.glob(str(candidate / "dt=*" / "*.parquet")))
        if files:
            return files
    return []


def read_rows(root: Path, principal: str, workspace: str, from_ms: int, to_ms: int) -> list[dict[str, Any]]:
    try:
        import duckdb  # type: ignore
    except ImportError as error:
        raise RuntimeError("duckdb Python package is required for the Phase 4 live audit") from error
    files = parquet_files(root, principal, workspace, "llm_tool_calls")
    if not files:
        return []
    connection = duckdb.connect(":memory:")
    try:
        cursor = connection.execute(
            f"""
            SELECT trace_id, llm_call_id, model_tool_call_id, tool_execution_id,
                   branch_id, source_surface, tool_family, tool_lineage_stage,
                   tool_lineage_stage_index, arguments_fingerprint, result_ref,
                   canonical_event_ref, related_execution_ids_json,
                   consumed_by_call_id, tool_outcome, tool_failure_owner,
                   tool_side_effect_state, tool_branch_state,
                   on_successful_path, same_tool_arguments_count,
                   observation_action_cycle_count, recovered_after_failure,
                   linkage_gap, observed_at_ms
            FROM read_parquet({sql_list(files)}, union_by_name = true, hive_partitioning = false)
            WHERE observed_at_ms >= ? AND observed_at_ms < ?
            ORDER BY observed_at_ms, record_revision
            LIMIT {MAX_AUDIT_ROWS + 1}
            """,
            [from_ms, to_ms],
        )
        columns = [item[0] for item in cursor.description]
        rows = cursor.fetchall()
        if len(rows) > MAX_AUDIT_ROWS:
            raise RuntimeError("tool-lineage audit row budget exceeded; narrow --window-hours")
        return [dict(zip(columns, row, strict=True)) for row in rows]
    finally:
        connection.close()


def read_lineage_capture_gaps(
    root: Path, principal: str, workspace: str, from_ms: int, to_ms: int
) -> list[dict[str, Any]]:
    try:
        import duckdb  # type: ignore
    except ImportError as error:
        raise RuntimeError("duckdb Python package is required for the Phase 4 live audit") from error
    files = parquet_files(root, principal, workspace, "llm_capture_gaps")
    if not files:
        return []
    connection = duckdb.connect(":memory:")
    try:
        cursor = connection.execute(
            f"""SELECT llm_call_id, gap_reason, missing_record_count, observed_at_ms
                FROM read_parquet({sql_list(files)}, union_by_name = true, hive_partitioning = false)
                WHERE (gap_reason LIKE 'tool_lineage_%'
                       OR gap_reason = 'runtime_transport_events_unclassified_due_broadcast_lag')
                  AND observed_at_ms >= ? AND observed_at_ms < ?
                ORDER BY observed_at_ms LIMIT {MAX_AUDIT_ROWS + 1}""",
            [from_ms, to_ms],
        )
        columns = [item[0] for item in cursor.description]
        rows = cursor.fetchall()
        if len(rows) > MAX_AUDIT_ROWS:
            raise RuntimeError("capture-gap audit row budget exceeded; narrow --window-hours")
        return [dict(zip(columns, row, strict=True)) for row in rows]
    finally:
        connection.close()


def read_producing_call_ids(
    root: Path, principal: str, workspace: str, from_ms: int, to_ms: int
) -> set[str]:
    try:
        import duckdb  # type: ignore
    except ImportError as error:
        raise RuntimeError("duckdb Python package is required for the Phase 4 live audit") from error
    files = parquet_files(root, principal, workspace, "llm_calls")
    if not files:
        return set()
    connection = duckdb.connect(":memory:")
    try:
        rows = connection.execute(
            f"""SELECT DISTINCT llm_call_id
                FROM read_parquet({sql_list(files)}, union_by_name = true, hive_partitioning = false)
                WHERE llm_call_id IS NOT NULL AND observed_at_ms >= ? AND observed_at_ms < ?
                LIMIT {MAX_AUDIT_ROWS + 1}""",
            [from_ms - 31 * 24 * 60 * 60 * 1000, to_ms],
        ).fetchall()
        if len(rows) > MAX_AUDIT_ROWS:
            raise RuntimeError("producing-call audit row budget exceeded; narrow --window-hours")
        return {str(row[0]) for row in rows}
    finally:
        connection.close()


def audit(
    rows: list[dict[str, Any]],
    capture_gaps: list[dict[str, Any]] | None = None,
    producing_call_ids: set[str] | None = None,
) -> dict[str, Any]:
    capture_gaps = capture_gaps or []
    violations: Counter[str] = Counter()
    stages: Counter[str] = Counter()
    outcomes: Counter[str] = Counter()
    owners: Counter[str] = Counter()
    surfaces: Counter[str] = Counter()
    families: Counter[str] = Counter()
    by_execution: dict[str, list[dict[str, Any]]] = defaultdict(list)
    by_call: dict[str, set[str]] = defaultdict(set)
    consumers: Counter[str] = Counter()
    related_execution_edges = 0
    gap_calls = {str(row["llm_call_id"]) for row in capture_gaps if row.get("llm_call_id")}
    gap_reasons = Counter(str(row.get("gap_reason") or "unknown") for row in capture_gaps)
    known_missing_lineage_revisions = sum(
        max(0, int(row.get("missing_record_count") or 0)) for row in capture_gaps
    )
    unowned_missing_lineage_revisions = sum(
        max(0, int(row.get("missing_record_count") or 0))
        for row in capture_gaps
        if not row.get("llm_call_id")
    )
    if unowned_missing_lineage_revisions:
        # An unowned loss could have contained a tool-lineage event, so a
        # strict Phase 4 cohort cannot claim complete causal coverage.
        violations["unowned_lineage_or_transport_capture_loss"] += (
            unowned_missing_lineage_revisions
        )

    for row in rows:
        stage = str(row.get("tool_lineage_stage") or "")
        stages[stage] += 1
        outcomes[str(row.get("tool_outcome") or "unknown")] += 1
        owners[str(row.get("tool_failure_owner") or "none")] += 1
        surfaces[str(row.get("source_surface") or "unknown")] += 1
        families[str(row.get("tool_family") or "unknown")] += 1
        identity = row.get("tool_execution_id")
        for field in ("trace_id", "llm_call_id", "model_tool_call_id", "tool_execution_id", "branch_id"):
            if not row.get(field):
                violations[f"missing_{field}"] += 1
        fingerprint = str(row.get("arguments_fingerprint") or "")
        if not re.fullmatch(r"[0-9a-f]{64}", fingerprint):
            violations["invalid_arguments_fingerprint"] += 1
        if stage not in REQUIRED_STAGES:
            violations["unknown_stage"] += 1
        if identity:
            identity = str(identity)
            expected_identity = (
                f"{row.get('llm_call_id')}:tool:{row.get('model_tool_call_id')}"
            )
            if identity != expected_identity:
                violations["invalid_tool_execution_identity"] += 1
            by_execution[identity].append(row)
            by_call[str(row.get("llm_call_id") or "")].add(identity)
            if stage == "result_consumed":
                consumers[identity] += 1
                if row.get("consumed_by_call_id") == row.get("llm_call_id"):
                    violations["result_consumed_by_producing_call"] += 1
        related = row.get("related_execution_ids_json")
        if related:
            try:
                parsed = json.loads(str(related))
            except (TypeError, ValueError):
                violations["invalid_related_execution_ids"] += 1
            else:
                if (
                    not isinstance(parsed, list)
                    or not parsed
                    or len(parsed) > 64
                    or not all(isinstance(item, str) and item for item in parsed)
                    or len(parsed) != len(set(parsed))
                ):
                    violations["invalid_related_execution_ids"] += 1
                else:
                    related_execution_edges += len(parsed)
                    if stage not in {"execution_finished", "branch_materialized"}:
                        violations["related_execution_ids_on_wrong_stage"] += 1

    executions_with_gap = 0
    executions_with_outcome = 0
    executions_with_producing_call = 0
    for execution_rows in by_execution.values():
        execution_stages = {str(row.get("tool_lineage_stage") or "") for row in execution_rows}
        has_capture_gap = any(str(row.get("llm_call_id") or "") in gap_calls for row in execution_rows)
        has_explicit_gap = "linkage_gap" in execution_stages or has_capture_gap
        llm_call_id = str(execution_rows[0].get("llm_call_id") or "")
        if producing_call_ids is not None:
            if llm_call_id in producing_call_ids:
                executions_with_producing_call += 1
            else:
                violations["tool_execution_missing_producing_call"] += 1
        outcome_by_stage = {
            str(row.get("tool_lineage_stage") or ""): str(row.get("tool_outcome") or "")
            for row in execution_rows
        }
        if "proposed" not in execution_stages and not has_explicit_gap:
            violations["execution_missing_proposal_or_gap"] += 1
        if has_explicit_gap:
            executions_with_gap += 1
        if execution_stages & OUTCOME_STAGES or has_capture_gap:
            executions_with_outcome += 1
        else:
            violations["execution_missing_authoritative_outcome_or_gap"] += 1
        if not has_explicit_gap and "branch_materialized" not in execution_stages:
            violations["execution_missing_branch_or_gap"] += 1
        if not has_capture_gap and "execution_started" in execution_stages and "execution_finished" not in execution_stages:
            violations["started_execution_missing_terminal_stage"] += 1
        if not has_capture_gap and "execution_finished" in execution_stages and "execution_started" not in execution_stages:
            violations["finished_execution_missing_start"] += 1
        if not has_capture_gap and "execution_finished" in execution_stages and "result_validated" not in execution_stages:
            violations["finished_execution_missing_result_validation"] += 1
        if not has_capture_gap and "result_consumed" in execution_stages and "result_validated" not in execution_stages:
            violations["consumed_result_missing_validation"] += 1
        if not has_capture_gap and outcome_by_stage.get("result_validated") == "succeeded" and "execution_finished" not in execution_stages:
            violations["successful_result_missing_execution"] += 1
        if not has_capture_gap and outcome_by_stage.get("branch_materialized") == "succeeded" and not {
            "execution_finished",
            "result_validated",
        }.issubset(execution_stages):
            violations["successful_branch_missing_execution_or_validation"] += 1
        if not has_capture_gap and "rollback_started" in execution_stages and "rollback_finished" not in execution_stages:
            violations["rollback_missing_terminal_stage"] += 1
        if not has_capture_gap and "rollback_started" in execution_stages and "execution_finished" not in execution_stages:
            violations["rollback_missing_tool_execution"] += 1

    return {
        "row_count": len(rows),
        "tool_execution_count": len(by_execution),
        "llm_call_count": len([key for key in by_call if key]),
        "multi_tool_call_count": sum(1 for value in by_call.values() if len(value) > 1),
        "executions_with_outcome": executions_with_outcome,
        "executions_with_producing_call": executions_with_producing_call,
        "executions_with_explicit_gap": executions_with_gap,
        "consumed_execution_count": len(consumers),
        "multi_consumer_execution_count": sum(1 for count in consumers.values() if count > 1),
        "related_execution_edge_count": related_execution_edges,
        "owned_capture_gap_call_count": len(gap_calls),
        "known_missing_lineage_revisions": known_missing_lineage_revisions,
        "unowned_missing_lineage_revisions": unowned_missing_lineage_revisions,
        "capture_gap_reason_counts": dict(sorted(gap_reasons.items())),
        "stage_counts": dict(sorted(stages.items())),
        "outcome_counts": dict(sorted(outcomes.items())),
        "failure_owner_counts": dict(sorted(owners.items())),
        "surface_counts": dict(sorted(surfaces.items())),
        "tool_family_counts": dict(sorted(families.items())),
        "violation_count": sum(violations.values()),
        "violation_counts": dict(sorted(violations.items())),
    }


def render_html(report: dict[str, Any]) -> str:
    audit_data = report["audit"]
    status = report["status"]
    tone = {"passed": "good", "failed": "bad", "skipped": "warn"}.get(status, "warn")
    violation_rows = "".join(
        f"<tr><td>{html.escape(key)}</td><td>{value:,}</td></tr>"
        for key, value in audit_data["violation_counts"].items()
    ) or '<tr><td colspan="2">None</td></tr>'
    return f"""<!doctype html><html><head><meta charset="utf-8"><title>LLM observability Phase 4</title>
<style>:root{{color-scheme:light dark;font-family:ui-sans-serif,system-ui}}body{{margin:0;background:#0b1020;color:#eef2ff}}main{{max-width:1040px;margin:auto;padding:32px}}.card{{background:#141b31;border:1px solid #2a3558;border-radius:14px;padding:18px;margin:14px 0}}.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px}}.metric{{background:#0f162b;border-radius:10px;padding:14px;color:#aeb9d8}}.metric b{{display:block;color:#fff;font-size:1.55rem}}.status{{display:inline-block;padding:5px 9px;border-radius:999px;font-weight:700}}.good{{background:#123f2d;color:#8ff0b8}}.bad{{background:#4b1f29;color:#ffadb9}}.warn{{background:#493817;color:#ffd88a}}table{{border-collapse:collapse;width:100%}}td,th{{padding:8px;text-align:left;border-bottom:1px solid #2a3558}}code{{color:#b8c8ff}}</style></head><body><main><h1>LLM observability Phase 4</h1><section class="card"><span class="status {tone}">{html.escape(status)}</span><p>Fact-only model-to-tool lineage audit. No IDs, arguments, results, prompts, or delegated content are included in this report.</p></section><section class="card grid"><div class="metric"><b>{audit_data['tool_execution_count']:,}</b>tool executions</div><div class="metric"><b>{audit_data['executions_with_producing_call']:,}</b>with producing call</div><div class="metric"><b>{audit_data['executions_with_outcome']:,}</b>with outcome/gap</div><div class="metric"><b>{audit_data['consumed_execution_count']:,}</b>results consumed</div><div class="metric"><b>{audit_data['related_execution_edge_count']:,}</b>delegation edges</div><div class="metric"><b>{audit_data['known_missing_lineage_revisions']:,}</b>explicitly missing revisions</div><div class="metric"><b>{audit_data['violation_count']:,}</b>violations</div></section><section class="card"><h2>Lifecycle stages</h2><p><code>{html.escape(json.dumps(audit_data['stage_counts'], sort_keys=True))}</code></p></section><section class="card"><h2>Explicit capture gaps</h2><p><code>{html.escape(json.dumps(audit_data['capture_gap_reason_counts'], sort_keys=True))}</code></p></section><section class="card"><h2>Surfaces</h2><p><code>{html.escape(json.dumps(audit_data['surface_counts'], sort_keys=True))}</code></p></section><section class="card"><h2>Violations</h2><table><thead><tr><th>Machine category</th><th>Count</th></tr></thead><tbody>{violation_rows}</tbody></table></section></main></body></html>"""


def write_report(output_dir: Path, report: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")


def self_test() -> None:
    base = {
        "trace_id": "trace",
        "llm_call_id": "call",
        "model_tool_call_id": "model-call",
        "tool_execution_id": "call:tool:model-call",
        "branch_id": "branch",
        "source_surface": "chat",
        "tool_family": "browser",
        "tool_lineage_stage_index": 0,
        "arguments_fingerprint": "a" * 64,
        "tool_outcome": "pending",
        "tool_failure_owner": None,
        "tool_side_effect_state": "none",
        "tool_branch_state": "active",
        "on_successful_path": None,
        "same_tool_arguments_count": 1,
        "observation_action_cycle_count": 0,
        "recovered_after_failure": False,
        "linkage_gap": None,
        "related_execution_ids_json": None,
        "consumed_by_call_id": None,
        "observed_at_ms": 1,
    }
    rows = [dict(base, tool_lineage_stage="proposed")]
    rows.append(dict(base, tool_lineage_stage="execution_started"))
    rows.append(dict(base, tool_lineage_stage="execution_finished", tool_outcome="succeeded"))
    rows.append(dict(base, tool_lineage_stage="result_validated", tool_outcome="succeeded"))
    rows.append(dict(base, tool_lineage_stage="branch_materialized", tool_outcome="succeeded"))
    result = audit(rows, producing_call_ids={"call"})
    assert result["violation_count"] == 0, result
    broken = audit(
        [dict(base, tool_lineage_stage="execution_finished", arguments_fingerprint="raw")],
        producing_call_ids=set(),
    )
    assert broken["violation_count"] >= 2, broken
    invented_identity = audit(
        [dict(base, tool_lineage_stage="proposed", tool_execution_id="invented")]
    )
    assert invented_identity["violation_counts"]["invalid_tool_execution_identity"] == 1
    self_consumed = audit(
        [
            dict(base, tool_lineage_stage="result_validated", tool_outcome="succeeded"),
            dict(
                base,
                tool_lineage_stage="result_consumed",
                tool_outcome="succeeded",
                consumed_by_call_id="call",
            ),
        ]
    )
    assert self_consumed["violation_counts"]["result_consumed_by_producing_call"] == 1
    wrong_edge = audit([
        dict(base, tool_lineage_stage="proposed", related_execution_ids_json='["child"]')
    ])
    assert wrong_edge["violation_counts"]["related_execution_ids_on_wrong_stage"] == 1
    unowned_gap = audit([], [{
        "llm_call_id": None,
        "gap_reason": "runtime_transport_events_unclassified_due_broadcast_lag",
        "missing_record_count": 2,
    }])
    assert unowned_gap["violation_counts"]["unowned_lineage_or_transport_capture_loss"] == 2


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime-root", type=Path, default=Path(os.environ.get("MAGICIAN_ROOT_DIR", "~/MagicianNotes")).expanduser())
    parser.add_argument("--principal", default=os.environ.get("LLM_PHASE4_PRINCIPAL", "anonymous"))
    parser.add_argument("--workspace", default=os.environ.get("LLM_PHASE4_WORKSPACE", "default"))
    parser.add_argument("--window-hours", type=float, default=24.0 * 7)
    parser.add_argument("--maturity-minutes", type=float, default=5.0)
    parser.add_argument("--output-dir", type=Path, default=Path("coverage/evals/llm-observability-phase4/latest"))
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("Phase 4 evaluator self-test passed")
        return 0
    validate_scope(args.principal, "principal")
    validate_scope(args.workspace, "workspace")
    if args.window_hours <= 0 or args.window_hours > 31 * 24:
        parser.error("--window-hours must be in (0, 744]")
    if args.maturity_minutes < 0 or args.maturity_minutes > 60:
        parser.error("--maturity-minutes must be in [0, 60]")
    if args.dry_run:
        print(json.dumps({"runtime_root": str(args.runtime_root), "scope": f"{args.principal}/{args.workspace}", "content_class": "fact_only"}, indent=2))
        return 0
    to_ms = int(time.time() * 1000 - args.maturity_minutes * 60_000)
    from_ms = to_ms - int(args.window_hours * 60 * 60 * 1000)
    try:
        rows = read_rows(args.runtime_root, args.principal, args.workspace, from_ms, to_ms)
        capture_gaps = read_lineage_capture_gaps(
            args.runtime_root, args.principal, args.workspace, from_ms, to_ms
        )
        producing_call_ids = read_producing_call_ids(
            args.runtime_root, args.principal, args.workspace, from_ms, to_ms
        )
        audit_data = audit(rows, capture_gaps, producing_call_ids)
    except Exception as error:
        print(f"Phase 4 lineage audit failed: {error}", file=sys.stderr)
        return 2
    status = "skipped" if not rows else "failed" if audit_data["violation_count"] else "passed"
    report = {
        "schema_version": 1,
        "evaluation": "llm-observability-phase4-tool-lineage",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "status": status,
        "scope": {"principal": args.principal, "workspace": args.workspace},
        "range": {"from_ms": from_ms, "to_ms": to_ms, "maturity_minutes": args.maturity_minutes},
        "audit": audit_data,
        "privacy": {"content_class": "fact_only", "stable_ids_written_to_report": False, "payloads_read": False},
    }
    write_report(args.output_dir, report)
    print(f"LLM observability Phase 4: {status}; executions={audit_data['tool_execution_count']}; violations={audit_data['violation_count']}")
    print(f"Report: {(args.output_dir / 'report.html').resolve().as_uri()}")
    if status == "skipped":
        return 3
    if status == "failed" and args.strict:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

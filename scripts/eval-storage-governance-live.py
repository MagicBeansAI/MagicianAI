#!/usr/bin/env python3
"""Live, isolated contract eval for storage inventory and guarded maintenance."""

from __future__ import annotations

import argparse
from dataclasses import dataclass, asdict
from html import escape
import json
import os
from pathlib import Path
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, build_opener


MAX_RESPONSE_BYTES = 2 * 1024 * 1024
EXPECTED_IDS = {
    "analytics_duckdb",
    "channel_assist_duckdb",
    "ui_threads_duckdb",
    "feed_duckdb",
    "attention_funnel_sqlite",
    "resurfacing_sqlite",
    "events",
    "memory_events",
    "llm_calls",
    "llm_embeddings",
    "llm_provider_attempts",
    "llm_tool_calls",
    "llm_capture_gaps",
    "llm_dispatch",
    "llm_call_io",
    "llm_context_blocks",
    "llm_content_tombstones",
    "llm_content_access_audit",
    "llm_trace_journal",
    "llm_restricted_journal",
    "compaction_metrics",
}


class EvalFailure(RuntimeError):
    pass


@dataclass(frozen=True)
class Gate:
    name: str
    passed: bool
    detail: str
    latency_ms: float = 0.0


def validate_snapshot(payload: Any, principal: str, workspace: str) -> list[Gate]:
    if not isinstance(payload, dict) or not isinstance(payload.get("entries"), list):
        raise EvalFailure("storage snapshot is not a typed object")
    entries = payload["entries"]
    by_id = {
        entry.get("id"): entry
        for entry in entries
        if isinstance(entry, dict) and isinstance(entry.get("id"), str)
    }
    missing = sorted(EXPECTED_IDS - set(by_id))
    mail = by_id.get("channel_assist_duckdb", {})
    tool_calls = by_id.get("llm_tool_calls", {})
    memory_events = by_id.get("memory_events", {})
    embeddings = by_id.get("llm_embeddings", {})
    journal = by_id.get("llm_trace_journal", {})
    metrics_entry = by_id.get("compaction_metrics", {})
    metrics = payload.get("compaction_metrics", {})
    mail_actions = {
        action.get("id")
        for action in mail.get("actions", [])
        if isinstance(action, dict)
    }
    return [
        Gate(
            "scope_identity",
            payload.get("principal") == principal and payload.get("workspace") == workspace,
            f"{payload.get('principal')}/{payload.get('workspace')}",
        ),
        Gate("complete_inventory", not missing, f"missing={missing or 'none'}"),
        Gate(
            "mail_lifecycle_boundary",
            mail.get("safety_class") == "lifecycle_managed"
            and mail.get("retention_days") is None
            and mail_actions == {"compact_channel_assist"},
            f"retention={mail.get('retention_days')} actions={sorted(mail_actions)}",
        ),
        Gate(
            "telemetry_retention_coverage",
            tool_calls.get("retention_days") == 90
            and memory_events.get("retention_days") == 90
            and embeddings.get("retention_days") == 90,
            "memory_events, llm_embeddings and llm_tool_calls are governed for 90 days",
        ),
        Gate(
            "journal_protection",
            journal.get("safety_class") == "authoritative"
            and journal.get("retention_days") is None
            and journal.get("actions") == [],
            "recovery journal has no generic destructive action",
        ),
        Gate(
            "non_negative_sizes",
            all(
                isinstance(entry.get(field), int) and entry[field] >= 0
                for entry in entries
                if isinstance(entry, dict)
                for field in ("size_bytes", "allocated_bytes", "wal_bytes", "file_count")
            ),
            f"validated {len(entries)} entries",
        ),
        Gate(
            "bounded_compaction_metrics",
            isinstance(metrics, dict)
            and isinstance(metrics.get("event_count"), int)
            and isinstance(metrics.get("retained_event_limit"), int)
            and 0 <= metrics["event_count"] <= metrics["retained_event_limit"] <= 1024
            and isinstance(metrics.get("storage_bytes"), int)
            and metrics["storage_bytes"] >= 0
            and metrics_entry.get("size_bytes") == metrics.get("storage_bytes")
            and metrics_entry.get("row_count") == metrics.get("event_count")
            and isinstance(metrics.get("areas"), list)
            and isinstance(metrics.get("recent_events"), list),
            f"events={metrics.get('event_count')} limit={metrics.get('retained_event_limit')} bytes={metrics.get('storage_bytes')}",
        ),
    ]


def validate_report(payload: Any, operation: str) -> list[Gate]:
    if not isinstance(payload, dict):
        raise EvalFailure(f"{operation} returned a non-object report")
    started = payload.get("started_at_ms")
    completed = payload.get("completed_at_ms")
    gates = [
        Gate(
            f"{operation}_timestamps",
            isinstance(started, int) and isinstance(completed, int) and completed >= started,
            f"started={started} completed={completed}",
        )
    ]
    if operation == "duckdb":
        reports = payload.get("duckdb")
        gates.append(
            Gate(
                "duckdb_all_owners_compacted",
                isinstance(reports, list)
                and len(reports) == 3
                and all(
                    isinstance(report, dict)
                    and report.get("bytes_after", -1) >= 0
                    and report.get("row_count", -1) >= 0
                    for report in reports
                ),
                f"reports={len(reports) if isinstance(reports, list) else 'invalid'}",
            )
        )
    elif operation == "parquet":
        stats = payload.get("parquet")
        gates.append(
            Gate(
                "parquet_verified_stats",
                isinstance(stats, dict)
                and all(
                    isinstance(stats.get(field), int) and stats[field] >= 0
                    for field in (
                        "partitions_scanned",
                        "partitions_compacted",
                        "raw_files_pruned",
                        "rows_compacted",
                    )
                ),
                "verified compaction counters returned",
            )
        )
        canonical = payload.get("canonical_llm")
        gates.append(
            Gate(
                "canonical_llm_rolling_stats",
                isinstance(canonical, dict)
                and all(
                    isinstance(canonical.get(field), int) and canonical[field] >= 0
                    for field in (
                        "partitions_scanned",
                        "partitions_compacted",
                        "partitions_with_bounded_tail",
                        "raw_files_compacted",
                        "raw_tail_files_visible",
                        "rows_compacted",
                    )
                ),
                "active-day rolling and bounded-tail counters returned",
            )
        )
    elif operation == "retention":
        stats = payload.get("retention")
        gates.append(
            Gate(
                "retention_verified_stats",
                isinstance(stats, dict)
                and all(
                    isinstance(stats.get(field), int) and stats[field] >= 0
                    for field in ("partitions_scanned", "partitions_removed", "bytes_removed")
                ),
                "scope-bound retention counters returned",
            )
        )
    return gates


class Client:
    def __init__(self, base_url: str, timeout: float):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.opener = build_opener()

    def request(
        self, method: str, path: str, body: dict[str, Any] | None = None
    ) -> tuple[int, Any, float]:
        data = json.dumps(body).encode() if body is not None else None
        request = Request(
            f"{self.base_url}{path}",
            data=data,
            method=method,
            headers={
                "Accept": "application/json",
                "Content-Type": "application/json",
                **(
                    {"Authorization": f"Bearer {os.environ['MAGICIAN_BEARER_TOKEN'].strip()}"}
                    if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
                    else {}
                ),
            },
        )
        started = time.perf_counter()
        try:
            response = self.opener.open(request, timeout=self.timeout)
            status = response.status
            raw = response.read(MAX_RESPONSE_BYTES + 1)
        except HTTPError as error:
            status = error.code
            raw = error.read(MAX_RESPONSE_BYTES + 1)
        except (URLError, TimeoutError) as error:
            raise EvalFailure(f"storage API unavailable: {error}") from error
        latency_ms = (time.perf_counter() - started) * 1000
        if len(raw) > MAX_RESPONSE_BYTES:
            raise EvalFailure("storage API response exceeded the eval byte cap")
        try:
            payload = json.loads(raw) if raw else None
        except json.JSONDecodeError as error:
            raise EvalFailure(f"storage API returned invalid JSON (HTTP {status})") from error
        return status, payload, latency_ms


def run_live(client: Client) -> list[Gate]:
    gates: list[Gate] = []
    status, snapshot, latency = client.request("GET", "/api/magician/v2/storage")
    if status != 200:
        raise EvalFailure(f"storage inventory failed with HTTP {status}: {snapshot}")
    gates.extend(
        Gate(gate.name, gate.passed, gate.detail, latency)
        for gate in validate_snapshot(snapshot, client.principal, client.workspace)
    )

    status, mismatch, latency = client.request(
        "POST",
        "/api/magician/v2/storage/actions/compact-databases",
        {"targets": ["analytics"], "confirmation": "yes"},
    )
    gates.append(
        Gate(
            "exact_confirmation_enforced",
            status == 400 and isinstance(mismatch, dict)
            and mismatch.get("error") == "confirmation_mismatch",
            f"HTTP {status}",
            latency,
        )
    )

    status, report, latency = client.request(
        "POST",
        "/api/magician/v2/storage/actions/compact-databases",
        {
            "targets": ["analytics", "channel_assist", "ui_threads"],
            "confirmation": "COMPACT DATABASE",
        },
    )
    if status != 200:
        raise EvalFailure(f"isolated DuckDB compaction failed with HTTP {status}: {report}")
    gates.extend(
        Gate(gate.name, gate.passed, gate.detail, latency)
        for gate in validate_report(report, "duckdb")
    )

    status, report, latency = client.request(
        "POST",
        "/api/magician/v2/storage/actions/compact-parquet",
        {"confirmation": "COMPACT PARQUET"},
    )
    if status != 200:
        raise EvalFailure(f"isolated Parquet compaction failed with HTTP {status}: {report}")
    gates.extend(
        Gate(gate.name, gate.passed, gate.detail, latency)
        for gate in validate_report(report, "parquet")
    )

    status, report, latency = client.request(
        "POST",
        "/api/magician/v2/storage/actions/apply-retention",
        {"retention_days": 90, "confirmation": "APPLY RETENTION"},
    )
    if status != 200:
        raise EvalFailure(f"isolated retention failed with HTTP {status}: {report}")
    gates.extend(
        Gate(gate.name, gate.passed, gate.detail, latency)
        for gate in validate_report(report, "retention")
    )

    status, snapshot, latency = client.request("GET", "/api/magician/v2/storage")
    metrics = snapshot.get("compaction_metrics", {}) if isinstance(snapshot, dict) else {}
    gates.append(
        Gate(
            "compaction_metrics_record_manual_runs",
            status == 200
            and isinstance(metrics, dict)
            and isinstance(metrics.get("event_count"), int)
            and metrics["event_count"] >= 3
            and isinstance(metrics.get("areas"), list)
            and any(
                isinstance(area, dict) and area.get("kind") == "duck_db"
                for area in metrics["areas"]
            ),
            f"HTTP {status} events={metrics.get('event_count')} areas={len(metrics.get('areas', [])) if isinstance(metrics.get('areas'), list) else 'invalid'}",
            latency,
        )
    )

    status, mismatch, latency = client.request(
        "POST",
        "/api/magician/v2/storage/actions/clear-compaction-metrics",
        {"confirmation": "CLEAR EVERYTHING"},
    )
    gates.append(
        Gate(
            "metrics_clear_exact_confirmation",
            status == 400
            and isinstance(mismatch, dict)
            and mismatch.get("error") == "confirmation_mismatch",
            f"HTTP {status}",
            latency,
        )
    )
    status, cleared, latency = client.request(
        "POST",
        "/api/magician/v2/storage/actions/clear-compaction-metrics",
        {"confirmation": "CLEAR COMPACTION METRICS"},
    )
    gates.append(
        Gate(
            "metrics_clear_isolated",
            status == 200
            and isinstance(cleared, dict)
            and cleared.get("event_count") == 0
            and cleared.get("storage_bytes") == 0
            and cleared.get("areas") == []
            and cleared.get("recent_events") == [],
            f"HTTP {status} events={cleared.get('event_count') if isinstance(cleared, dict) else 'invalid'}",
            latency,
        )
    )
    return gates


def write_report(output_dir: Path, gates: list[Gate], mode: str) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        "schema_version": 1,
        "eval": "storage-governance",
        "mode": mode,
        "passed": all(gate.passed for gate in gates),
        "gate_count": len(gates),
        "gates": [asdict(gate) for gate in gates],
    }
    (output_dir / "report.json").write_text(
        json.dumps(payload, indent=2) + "\n", encoding="utf-8"
    )
    rows = "".join(
        "<tr><td>{}</td><td class='{}'>{}</td><td>{}</td><td>{:.1f} ms</td></tr>".format(
            escape(gate.name),
            "pass" if gate.passed else "fail",
            "PASS" if gate.passed else "FAIL",
            escape(gate.detail),
            gate.latency_ms,
        )
        for gate in gates
    )
    html = f"""<!doctype html><html><head><meta charset="utf-8"><title>Storage governance live eval</title>
<style>body{{font:15px system-ui;margin:2rem;color:#17202a}}table{{border-collapse:collapse;width:100%}}th,td{{border-bottom:1px solid #ddd;padding:.65rem;text-align:left}}.pass{{color:#16803b;font-weight:700}}.fail{{color:#c0392b;font-weight:700}}</style></head>
<body><h1>Storage governance live eval</h1><p>Mode: {escape(mode)} · {len(gates)} contract gates</p><table><thead><tr><th>Gate</th><th>Result</th><th>Evidence</th><th>Latency</th></tr></thead><tbody>{rows}</tbody></table></body></html>"""
    (output_dir / "report.html").write_text(html, encoding="utf-8")


def synthetic_gates() -> list[Gate]:
    entries = []
    for identifier in sorted(EXPECTED_IDS):
        entry = {
            "id": identifier,
            "size_bytes": 0,
            "allocated_bytes": 0,
            "wal_bytes": 0,
            "file_count": 0,
            "actions": [],
            "retention_days": None,
            "safety_class": "observability",
        }
        entries.append(entry)
    by_id = {entry["id"]: entry for entry in entries}
    by_id["channel_assist_duckdb"].update(
        safety_class="lifecycle_managed",
        actions=[{"id": "compact_channel_assist"}],
    )
    by_id["memory_events"]["retention_days"] = 90
    by_id["llm_embeddings"]["retention_days"] = 90
    by_id["llm_tool_calls"]["retention_days"] = 90
    by_id["llm_trace_journal"]["safety_class"] = "authoritative"
    by_id["compaction_metrics"].update(size_bytes=0, row_count=0)
    return validate_snapshot(
        {
            "principal": "owner",
            "workspace": "default",
            "entries": entries,
            "compaction_metrics": {
                "event_count": 0,
                "retained_event_limit": 256,
                "storage_bytes": 0,
                "areas": [],
                "recent_events": [],
            },
        },
        "owner",
        "default",
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--api-base-url", default="http://127.0.0.1:3002")
    parser.add_argument("--timeout-secs", type=float, default=60.0)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        gates = synthetic_gates()
        mode = "self-test"
    elif args.dry_run:
        gates = [Gate("request_plan", True, "isolated scope inventory + guarded mutation matrix")]
        mode = "dry-run"
    else:
        mode = "live"
        try:
            gates = run_live(
                Client(
                    args.api_base_url,
                    args.timeout_secs,
                )
            )
        except EvalFailure as error:
            # A failed live contract is still an eval result. Persist it so the
            # aggregate HTML report can link to the evidence instead of losing
            # the child report to a traceback-only exit.
            gates = [Gate("live_contract", False, str(error))]
    if args.output_dir:
        write_report(args.output_dir, gates, mode)
        print(f"Report: {(args.output_dir.resolve() / 'report.html').as_uri()}")
    for gate in gates:
        print(f"{'PASS' if gate.passed else 'FAIL'} {gate.name}: {gate.detail}")
    return 0 if all(gate.passed for gate in gates) else 1


if __name__ == "__main__":
    raise SystemExit(main())

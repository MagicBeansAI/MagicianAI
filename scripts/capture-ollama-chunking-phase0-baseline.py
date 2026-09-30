#!/usr/bin/env python3
"""Capture the chunking Phase 0 baseline from existing dispatch Parquet.

This script is read-only with respect to Magician runtime data and never calls
an LLM. It prints JSON to stdout unless --output is supplied.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from collections import defaultdict
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any, Iterable

import duckdb


OPERATIONS = (
    "memory_temperature_utility_review",
    "memory_episode_quality_classification",
    "memory_entity_extraction",
    "distill_evidence",
    "memory_environment_knowledge_extraction",
    "memory_archive_summary",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--runtime-root",
        type=Path,
        default=Path.home() / "MagicianNotes",
        help="MAGICIAN_ROOT_DIR containing scopes/ and llm_pricing.json",
    )
    parser.add_argument("--from-date", required=True, help="inclusive YYYY-MM-DD")
    parser.add_argument("--to-date", required=True, help="inclusive YYYY-MM-DD")
    parser.add_argument(
        "--routing-baseline",
        type=Path,
        default=Path("data/magician_v2/llm_chunking_evals/phase0-routing-baseline-v1.json"),
    )
    parser.add_argument("--pricing-file", type=Path)
    parser.add_argument("--output", type=Path, help="optional JSON output path")
    return parser.parse_args()


def validate_date(raw: str) -> str:
    return date.fromisoformat(raw).isoformat()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def parquet_scan(glob: Path) -> str:
    escaped = glob.as_posix().replace("'", "''")
    return (
        f"read_parquet('{escaped}', union_by_name=true, "
        "hive_partitioning=true)"
    )


def quantile(values: Iterable[int | float | None], fraction: float) -> float | None:
    ordered = sorted(float(value) for value in values if value is not None)
    if not ordered:
        return None
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return round(ordered[lower], 1)
    weight = position - lower
    return round(ordered[lower] * (1.0 - weight) + ordered[upper] * weight, 1)


def utc_date(timestamp_ms: int) -> date:
    return datetime.fromtimestamp(timestamp_ms / 1000, timezone.utc).date()


def load_rates(path: Path) -> list[dict[str, Any]]:
    value = json.loads(path.read_text(encoding="utf-8"))
    return list(value["rates"])


def resolve_rate(
    rates: list[dict[str, Any]], provider: str, model: str, at: date
) -> dict[str, Any] | None:
    candidates = []
    provider_key = provider.strip().lower()
    for rate in rates:
        if str(rate["provider"]).strip().lower() != provider_key:
            continue
        prefix = str(rate.get("model_prefix", ""))
        if not model.startswith(prefix):
            continue
        effective = date.fromisoformat(rate["effective_from"])
        if effective <= at:
            candidates.append((len(prefix), effective, rate))
    if not candidates:
        return None
    return max(candidates, key=lambda candidate: (candidate[0], candidate[1]))[2]


def estimate_cost(row: dict[str, Any], rates: list[dict[str, Any]]) -> float | None:
    provider = row.get("provider")
    model = row.get("model")
    timestamp_ms = row.get("timestamp_ms")
    if not provider or not model or timestamp_ms is None:
        return None
    rate = resolve_rate(rates, provider, model, utc_date(int(timestamp_ms)))
    if rate is None:
        return None

    prompt = max(int(row.get("prompt_tokens") or 0), 0)
    cached = min(max(int(row.get("cached_tokens") or 0), 0), prompt)
    completion = max(int(row.get("completion_tokens") or 0), 0)
    uncached = prompt - cached

    input_multiplier = 1.0
    output_multiplier = 1.0
    long_context = rate.get("long_context")
    if long_context and prompt > int(long_context["threshold_tokens"]):
        input_multiplier = float(long_context["input_multiplier"])
        output_multiplier = float(long_context["output_multiplier"])

    input_rate = float(rate["input_per_m"])
    cached_rate = float(rate.get("cache_read_per_m", input_rate))
    output_rate = float(rate["output_per_m"])
    return (
        (uncached * input_rate + cached * cached_rate) * input_multiplier
        + completion * output_rate * output_multiplier
    ) / 1_000_000.0


def fetch_rows(
    connection: duckdb.DuckDBPyConnection,
    scan: str,
    from_date: str,
    to_date: str,
) -> list[dict[str, Any]]:
    columns = (
        "timestamp_ms, operation, success, provider, model, wait_ms, execution_ms, "
        "attempts, prompt_tokens, completion_tokens, cached_tokens, reasoning_tokens"
    )
    query = (
        f"SELECT {columns} FROM {scan} "
        "WHERE operation IN (SELECT * FROM unnest(?)) "
        "AND dt >= CAST(? AS DATE) AND dt <= CAST(? AS DATE)"
    )
    cursor = connection.execute(query, [list(OPERATIONS), from_date, to_date])
    names = [description[0] for description in cursor.description]
    return [dict(zip(names, row)) for row in cursor.fetchall()]


def queue_slice(rows: list[dict[str, Any]]) -> dict[str, Any]:
    if not rows:
        return {
            "status": "not_observed",
            "observations": 0,
            "successes": None,
            "p50_wait_ms": None,
            "p90_wait_ms": None,
            "p99_wait_ms": None,
            "max_wait_ms": None,
        }
    waits = [row["wait_ms"] for row in rows]
    observed_waits = [int(wait) for wait in waits if wait is not None]
    return {
        "status": "observed",
        "observations": len(rows),
        "successes": sum(1 for row in rows if row["success"]),
        "p50_wait_ms": quantile(waits, 0.50),
        "p90_wait_ms": quantile(waits, 0.90),
        "p99_wait_ms": quantile(waits, 0.99),
        "max_wait_ms": max(observed_waits) if observed_waits else None,
    }


def fetch_ollama_queue_rows(
    connection: duckdb.DuckDBPyConnection,
    scan: str,
    from_date: str,
    to_date: str,
) -> dict[str, list[dict[str, Any]]]:
    query = (
        f"SELECT priority, success, wait_ms FROM {scan} "
        "WHERE provider = ? AND dt >= CAST(? AS DATE) AND dt <= CAST(? AS DATE)"
    )
    cursor = connection.execute(query, ["Ollama", from_date, to_date])
    names = [description[0] for description in cursor.description]
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for raw in cursor.fetchall():
        row = dict(zip(names, raw))
        grouped[str(row["priority"])].append(row)
    return grouped


def profile_models(routing: dict[str, Any]) -> dict[str, tuple[str | None, str | None]]:
    profiles = routing["current_cloud_baseline_profiles"]
    result = {}
    for operation in routing["operations"]:
        primary = profiles.get(
            operation["current_cloud_baseline_profile"], {}
        ).get("model")
        fallback = profiles.get(
            operation["current_cloud_retry_profile"], {}
        ).get("model")
        result[operation["operation"]] = (primary, fallback)
    return result


def aggregate_operations(
    rows: list[dict[str, Any]],
    rates: list[dict[str, Any]],
    routing: dict[str, Any],
) -> list[dict[str, Any]]:
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        grouped[str(row["operation"])].append(row)
    models_by_operation = profile_models(routing)

    results = []
    for operation in OPERATIONS:
        operation_rows = grouped.get(operation, [])
        if not operation_rows:
            results.append(
                {
                    "operation": operation,
                    "status": "not_observed",
                    "calls": 0,
                    "successes": 0,
                    "failures": 0,
                    "notes": [],
                }
            )
            continue

        costs = [estimate_cost(row, rates) for row in operation_rows]
        known_costs = [cost for cost in costs if cost is not None]
        primary_model, fallback_model = models_by_operation.get(operation, (None, None))
        fallback_calls = None
        notes = []
        if fallback_model and fallback_model != primary_model:
            fallback_calls = sum(1 for row in operation_rows if row["model"] == fallback_model)
        elif fallback_model:
            notes.append(
                "Primary and retry use the same model; profile-level fallback is not distinguishable in dispatch rows."
            )

        def total(field: str) -> int:
            return sum(int(row.get(field) or 0) for row in operation_rows)

        results.append(
            {
                "operation": operation,
                "status": "observed",
                "models": sorted({str(row["model"]) for row in operation_rows if row["model"]}),
                "calls": len(operation_rows),
                "successes": sum(1 for row in operation_rows if row["success"]),
                "failures": sum(1 for row in operation_rows if not row["success"]),
                "retried_jobs": sum(1 for row in operation_rows if int(row["attempts"] or 0) > 1),
                "physical_attempts": total("attempts"),
                "prompt_tokens": total("prompt_tokens"),
                "cached_tokens": total("cached_tokens"),
                "completion_tokens": total("completion_tokens"),
                "reasoning_tokens": total("reasoning_tokens"),
                "p50_prompt_tokens": quantile(
                    (row["prompt_tokens"] for row in operation_rows), 0.50
                ),
                "p90_prompt_tokens": quantile(
                    (row["prompt_tokens"] for row in operation_rows), 0.90
                ),
                "max_prompt_tokens": max(
                    int(row["prompt_tokens"] or 0) for row in operation_rows
                ),
                "p50_latency_ms": quantile(
                    (row["execution_ms"] for row in operation_rows), 0.50
                ),
                "p90_latency_ms": quantile(
                    (row["execution_ms"] for row in operation_rows), 0.90
                ),
                "estimated_cost_usd": (
                    round(sum(known_costs), 6) if len(known_costs) == len(costs) else None
                ),
                "schema_valid_rate": None,
                "fallback_calls": fallback_calls,
                "notes": notes,
            }
        )
    return results


def main() -> int:
    args = parse_args()
    from_date = validate_date(args.from_date)
    to_date = validate_date(args.to_date)
    if from_date > to_date:
        raise SystemExit("--from-date must be before or equal to --to-date")

    routing_path = args.routing_baseline.resolve()
    pricing_path = (args.pricing_file or args.runtime_root / "llm_pricing.json").resolve()
    routing = json.loads(routing_path.read_text(encoding="utf-8"))
    rates = load_rates(pricing_path)

    analytics_root = (
        args.runtime_root / "scopes" / "anonymous" / "default" / "analytics"
    )
    dispatch_glob = analytics_root / "llm_dispatch" / "dt=*" / "*.parquet"
    if not list((analytics_root / "llm_dispatch").glob("dt=*/*.parquet")):
        raise SystemExit(f"no dispatch Parquet files found under {analytics_root}")

    connection = duckdb.connect()
    scan = parquet_scan(dispatch_glob)
    rows = fetch_rows(connection, scan, from_date, to_date)
    queue_rows = fetch_ollama_queue_rows(connection, scan, from_date, to_date)
    operations = aggregate_operations(rows, rates, routing)

    report = {
        "schema_version": 1,
        "report_id": f"ollama-logical-context-phase0-telemetry-{to_date}",
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "status": "historical_snapshot",
        "baseline_role": "pre_migration_cloud_comparison_only",
        "implementation_target": routing["implementation_target"],
        "source": {
            "kind": "existing_parquet",
            "window_start": from_date,
            "window_end": to_date,
            "fixture_suite": None,
            "repeat_count": None,
            "lakehouse": "<MAGICIAN_ROOT_DIR>/scopes/anonymous/default/analytics/llm_dispatch/dt=*/*.parquet",
            "query_behavior": "union_by_name=true, hive_partitioning=true",
            "provider_calls_made_for_capture": 0,
            "pricing_file_sha256": sha256(pricing_path),
        },
        "totals": {
            "calls": len(rows),
            "successes": sum(1 for row in rows if row["success"]),
            "failures": sum(1 for row in rows if not row["success"]),
            "prompt_tokens": sum(int(row["prompt_tokens"] or 0) for row in rows),
            "cached_tokens": sum(int(row["cached_tokens"] or 0) for row in rows),
            "completion_tokens": sum(int(row["completion_tokens"] or 0) for row in rows),
            "reasoning_tokens": sum(int(row["reasoning_tokens"] or 0) for row in rows),
            "estimated_cost_usd": round(
                sum(
                    cost
                    for row in rows
                    if (cost := estimate_cost(row, rates)) is not None
                ),
                6,
            ),
        },
        "operations": operations,
        "queue_latency": {
            "high_priority_ollama": queue_slice(queue_rows.get("high", [])),
            "normal_priority_ollama_proxy": queue_slice(queue_rows.get("normal", [])),
            "background_ollama": queue_slice(queue_rows.get("background", [])),
        },
        "limitations": [
            "This command reads historical telemetry and makes no provider calls.",
            "Transport success does not prove final domain-schema validity.",
            "No semantic-quality or chunk-boundary judgement is inferred from dispatch rows.",
        ],
        "production_behavior_changed": False,
    }

    rendered = json.dumps(report, indent=2, sort_keys=False) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

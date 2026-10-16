#!/usr/bin/env python3
"""Live, content-free Phase 2F activation and reconciliation audit.

The evaluator compares canonical journal-materialized call completions with the
temporary legacy compatibility mirror by stable ``llm_call_id``. It also checks
that captured final-attempt facts plus explicit unavailable-attempt gaps account
for every provider-attempt ordinal, joins queued calls to their scoped dispatch
timing by stable job id, verifies pricing/timing fields, and probes the governed
overview API. Prompt, response, reasoning summary, tool, message,
attachment, transcript, and artifact content is never selected.
"""

from __future__ import annotations

import argparse
import html
import json
import math
import os
import sys
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable


CALL_FIELDS = (
    "principal",
    "workspace",
    "llm_call_id",
    "dispatch_job_id",
    "provider_attempt_count",
    "input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "audio_input_tokens",
    "audio_output_tokens",
    "audio_cached_tokens",
    "total_tokens",
    "input_cost_usd",
    "output_cost_usd",
    "reasoning_cost_usd",
    "cache_cost_usd",
    "cost_usd",
    "latency_ms",
    "call_terminal_state",
    "transport_success",
    "success",
    "operation",
    "response_kind",
    "error_class",
    "error_code",
    "finish_reason",
    "validation_error_class",
    "discard_reason",
    "training_exclusion_reason",
    "pricing_version",
    "cost_source",
    "observed_at_ms",
)
LEGACY_FIELDS = (
    "principal",
    "workspace",
    "llm_call_id",
    "dispatch_job_id",
    "provider_attempt_count",
    "input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "cost_usd",
    "latency_ms",
    "timestamp_ms",
    "response_reused",
    "response_kind",
)
DISPATCH_FIELDS = (
    "principal",
    "workspace",
    "job_id",
    "llm_call_id",
    "wait_ms",
    "execution_ms",
    "local_prep_ms",
    "state",
    "response_reused",
    "completed_at_ms",
    "timestamp_ms",
)
INTEGER_PARITY_FIELDS = (
    "provider_attempt_count",
    "input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "latency_ms",
)
ATTEMPT_INTEGER_PARITY_FIELDS = (
    "input_tokens",
    "output_tokens",
    "reasoning_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "audio_input_tokens",
    "audio_output_tokens",
    "audio_cached_tokens",
    "total_tokens",
)
ATTEMPT_COST_PARITY_FIELDS = (
    "input_cost_usd",
    "output_cost_usd",
    "reasoning_cost_usd",
    "cache_cost_usd",
    "cost_usd",
)
ATTEMPT_GAP_REASON = "earlier_provider_attempt_lifecycle_unavailable"
TERMINAL_ATTEMPT_GAP_REASON = "terminal_provider_attempt_lifecycle_unavailable"
UNCLASSIFIED_TRANSPORT_LAG_REASONS = {
    "runtime_transport_events_unclassified_due_broadcast_lag",
    "llm_dispatch_events_unclassified_due_broadcast_lag",
}
KNOWN_MISSING_FACT_REASONS = {
    ATTEMPT_GAP_REASON,
    TERMINAL_ATTEMPT_GAP_REASON,
    "critical_buffer_saturated",
    "critical_worker_unavailable",
}
PRICING_ROW_VERSION_PREFIX = "pricing-row-v1:"
RUNTIME_MIRROR_EXACT_PARITY_PRICING_VERSIONS = {
    "runtime-pricing-miss@call-time",
    "runtime-pricing-invalid-value@call-time",
}
RUNTIME_MIRROR_EXPECTED_PRICING_VERSIONS = (
    RUNTIME_MIRROR_EXACT_PARITY_PRICING_VERSIONS
    | {
        "provider-usage-unreported@call-time",
        "logical-chunk-summary-non-billing-v1",
    }
)


def runtime_mirror_exact_parity_pricing(version: Any) -> bool:
    value = str(version or "")
    return value.startswith(PRICING_ROW_VERSION_PREFIX) or (
        value in RUNTIME_MIRROR_EXACT_PARITY_PRICING_VERSIONS
    )


def runtime_mirror_expected_pricing(version: Any) -> bool:
    return runtime_mirror_exact_parity_pricing(version) or (
        str(version or "") in RUNTIME_MIRROR_EXPECTED_PRICING_VERSIONS
    )


FORBIDDEN_CONTENT_FIELDS = (
    "prompt",
    "response",
    "messages",
    "reasoning_summary",
    "tool_arguments",
    "tool_results",
    "attachments",
    "transcript",
    "artifact",
)
FIELD_TYPES = {
    "principal": "VARCHAR",
    "workspace": "VARCHAR",
    "llm_call_id": "VARCHAR",
    "dispatch_job_id": "VARCHAR",
    "provider_attempt_id": "VARCHAR",
    "provider_attempt_index": "BIGINT",
    "job_id": "VARCHAR",
    "provider_attempt_count": "BIGINT",
    "input_tokens": "BIGINT",
    "output_tokens": "BIGINT",
    "reasoning_tokens": "BIGINT",
    "cache_read_tokens": "BIGINT",
    "cache_creation_tokens": "BIGINT",
    "audio_input_tokens": "BIGINT",
    "audio_output_tokens": "BIGINT",
    "audio_cached_tokens": "BIGINT",
    "total_tokens": "BIGINT",
    "input_cost_usd": "DOUBLE",
    "output_cost_usd": "DOUBLE",
    "reasoning_cost_usd": "DOUBLE",
    "cache_cost_usd": "DOUBLE",
    "cost_usd": "DOUBLE",
    "latency_ms": "BIGINT",
    "call_terminal_state": "VARCHAR",
    "attempt_terminal_state": "VARCHAR",
    "transport_success": "BOOLEAN",
    "success": "BOOLEAN",
    "operation": "VARCHAR",
    "pricing_version": "VARCHAR",
    "cost_source": "VARCHAR",
    "observed_at_ms": "BIGINT",
    "timestamp_ms": "BIGINT",
    "response_reused": "BOOLEAN",
    "response_kind": "VARCHAR",
    "error_class": "VARCHAR",
    "error_code": "VARCHAR",
    "finish_reason": "VARCHAR",
    "validation_error_class": "VARCHAR",
    "discard_reason": "VARCHAR",
    "training_exclusion_reason": "VARCHAR",
    "state": "VARCHAR",
    "wait_ms": "BIGINT",
    "execution_ms": "BIGINT",
    "local_prep_ms": "BIGINT",
    "completed_at_ms": "BIGINT",
    "gap_reason": "VARCHAR",
    "missing_record_count": "BIGINT",
}


def sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def sql_files(files: Iterable[Path]) -> str:
    return "[" + ",".join(sql_string(str(path)) for path in files) + "]"


def safe_parquet_files(root: Path, pattern: str) -> list[Path]:
    """Discover regular Parquet files without traversing symlinked scope data."""
    if not root.exists():
        return []
    if root.is_symlink() or not root.is_dir():
        raise RuntimeError(f"analytics dataset root is not a real directory: {root}")
    for partition in sorted(root.glob("dt=*")):
        if partition.is_symlink() or not partition.is_dir():
            raise RuntimeError(
                f"analytics partition is not a real directory: {partition}"
            )
    files: list[Path] = []
    for path in sorted(root.glob(pattern)):
        if path.parent.is_symlink() or path.is_symlink() or not path.is_file():
            raise RuntimeError(f"analytics source is not a regular scoped file: {path}")
        files.append(path)
    return files


def validate_scope_component(value: str, label: str) -> None:
    invalid = {'/', '\\', ':', '*', '?', '"', '<', '>', '|'}
    if (
        not value
        or value.strip() != value
        or len(value.encode("utf-8")) > 255
        or value in {".", ".."}
        or any(character in invalid or ord(character) < 32 for character in value)
    ):
        raise RuntimeError(f"invalid {label} scope component")


def is_machine_category(value: Any) -> bool:
    if value is None:
        return True
    text = str(value)
    return (
        bool(text)
        and text.strip() == text
        and len(text.encode("utf-8")) <= 128
        and all(
            character.isascii()
            and (character.isalnum() or character in "_-.:/")
            for character in text
        )
    )


def is_pricing_version(value: Any) -> bool:
    if value is None:
        return True
    text = str(value)
    if not (
        bool(text)
        and text.strip() == text
        and len(text.encode("utf-8")) <= 128
        and all(
            character.isascii()
            and (character.isalnum() or character in "_-.:/@")
            for character in text
        )
    ):
        return False
    if not text.startswith(PRICING_ROW_VERSION_PREFIX):
        return True
    fingerprint = text[len(PRICING_ROW_VERSION_PREFIX) :]
    return len(fingerprint) == 64 and all(
        character in "0123456789abcdef" for character in fingerprint
    )


def is_canonical_identifier(value: Any, *, required: bool) -> bool:
    if value is None:
        return not required
    text = str(value)
    return bool(text) and text.strip() == text


def ensure_no_symlink_components(base: Path, target: Path) -> None:
    """Reject scope redirection below the configured runtime root."""
    try:
        relative = target.relative_to(base)
    except ValueError as error:
        raise RuntimeError(f"analytics path escapes runtime root: {target}") from error
    current = base
    for component in relative.parts:
        current = current / component
        if not current.exists() and not current.is_symlink():
            return
        if current.is_symlink() or not current.is_dir():
            raise RuntimeError(
                f"analytics scope path contains a non-directory or symlink: {current}"
            )


def fetch_dicts(connection: Any, query: str) -> list[dict[str, Any]]:
    cursor = connection.execute(query)
    columns = [str(column[0]) for column in cursor.description]
    return [dict(zip(columns, row, strict=True)) for row in cursor.fetchall()]


def parquet_columns(connection: Any, files: list[Path]) -> set[str]:
    if not files:
        return set()
    rows = connection.execute(
        f"DESCRIBE SELECT * FROM read_parquet({sql_files(files)}, union_by_name=true)"
    ).fetchall()
    return {str(row[0]) for row in rows}


def typed_projection(
    fields: Iterable[str],
    columns: set[str],
    *,
    fallbacks: dict[str, str] | None = None,
) -> str:
    fallbacks = fallbacks or {}
    projected: list[str] = []
    for field in fields:
        if field in columns:
            projected.append(f'"{field}"')
        elif field in fallbacks:
            projected.append(f"{fallbacks[field]} AS \"{field}\"")
        else:
            projected.append(
                f"CAST(NULL AS {FIELD_TYPES[field]}) AS \"{field}\""
            )
    return ", ".join(projected)


def latest_by_call(rows: list[dict[str, Any]], timestamp_field: str) -> dict[str, dict[str, Any]]:
    latest: dict[str, dict[str, Any]] = {}
    for row in rows:
        call_id = str(row.get("llm_call_id") or "").strip()
        if not call_id:
            continue
        existing = latest.get(call_id)
        if existing is None or int(row.get(timestamp_field) or 0) >= int(
            existing.get(timestamp_field) or 0
        ):
            latest[call_id] = row
    return latest


def numbers_equal(field: str, left: Any, right: Any) -> bool:
    if left is None or right is None:
        return left is None and right is None
    if field.endswith("_cost_usd") or field == "cost_usd":
        left_value = float(left)
        right_value = float(right)
        return math.isclose(left_value, right_value, rel_tol=1e-7, abs_tol=1e-9)
    return int(left) == int(right)


def reconcile(
    canonical_rows: list[dict[str, Any]],
    legacy_rows: list[dict[str, Any]],
    attempts: list[dict[str, Any]],
    gaps: list[dict[str, Any]],
    dispatch_rows: list[dict[str, Any]],
    *,
    expected_principal: str | None = None,
    expected_workspace: str | None = None,
) -> dict[str, Any]:
    expected_scope = (
        (expected_principal, expected_workspace)
        if expected_principal is not None and expected_workspace is not None
        else None
    )
    scope_mismatches: list[dict[str, Any]] = []
    if expected_scope is not None:
        for dataset, rows in (
            ("canonical_calls", canonical_rows),
            ("compatibility_calls", legacy_rows),
            ("provider_attempts", attempts),
            ("capture_gaps", gaps),
            ("dispatch", dispatch_rows),
        ):
            for row in rows:
                actual_scope = (
                    str(row.get("principal") or ""),
                    str(row.get("workspace") or ""),
                )
                is_process_diagnostic = (
                    dataset == "capture_gaps"
                    and row.get("gap_reason") in UNCLASSIFIED_TRANSPORT_LAG_REASONS
                    and actual_scope == ("anonymous", "default")
                )
                if actual_scope != expected_scope and not is_process_diagnostic:
                    scope_mismatches.append(
                        {
                            "dataset": dataset,
                            "principal": actual_scope[0],
                            "workspace": actual_scope[1],
                        }
                    )

    category_violations: list[dict[str, str]] = []
    noncanonical_identity_fields: list[dict[str, str]] = []
    terminal_semantic_mismatches: list[dict[str, str]] = []
    categorical_fields = (
        (
            "canonical_calls",
            canonical_rows,
            (
                "response_kind",
                "error_class",
                "error_code",
                "finish_reason",
                "validation_error_class",
                "discard_reason",
                "training_exclusion_reason",
            ),
        ),
        ("compatibility_calls", legacy_rows, ("response_kind",)),
        (
            "provider_attempts",
            attempts,
            (
                "error_class",
                "error_code",
                "finish_reason",
                "training_exclusion_reason",
            ),
        ),
        ("capture_gaps", gaps, ("gap_reason",)),
    )
    for dataset, rows, fields in categorical_fields:
        for row in rows:
            for field in fields:
                if not is_machine_category(row.get(field)):
                    # Never copy the offending value into the report: it is
                    # precisely the content whose persistence is being audited.
                    category_violations.append({"dataset": dataset, "field": field})
    for dataset, rows in (
        ("canonical_calls", canonical_rows),
        ("provider_attempts", attempts),
    ):
        for row in rows:
            if not is_pricing_version(row.get("pricing_version")):
                category_violations.append(
                    {"dataset": dataset, "field": "pricing_version"}
                )
    for row in canonical_rows:
        state = str(row.get("call_terminal_state") or "")
        expected_success = state == "succeeded"
        if state not in {"succeeded", "failed", "cancelled", "tombstoned"}:
            terminal_semantic_mismatches.append(
                {"dataset": "canonical_calls", "field": "call_terminal_state"}
            )
        for field in ("transport_success", "success"):
            if not isinstance(row.get(field), bool) or row.get(field) != expected_success:
                terminal_semantic_mismatches.append(
                    {"dataset": "canonical_calls", "field": field}
                )
    for dataset, rows, required_fields, optional_fields in (
        ("canonical_calls", canonical_rows, ("llm_call_id",), ("dispatch_job_id",)),
        (
            "provider_attempts",
            attempts,
            ("llm_call_id", "provider_attempt_id"),
            (),
        ),
        ("capture_gaps", gaps, (), ("llm_call_id",)),
        ("dispatch", dispatch_rows, (), ("job_id", "llm_call_id")),
    ):
        for row in rows:
            for field in required_fields:
                if not is_canonical_identifier(row.get(field), required=True):
                    noncanonical_identity_fields.append(
                        {"dataset": dataset, "field": field}
                    )
            for field in optional_fields:
                if row.get(field) is not None and not is_canonical_identifier(
                    row.get(field), required=False
                ):
                    noncanonical_identity_fields.append(
                        {"dataset": dataset, "field": field}
                    )

    canonical_identity_counts: dict[str, int] = {}
    canonical_missing_identity_count = 0
    for row in canonical_rows:
        call_id = str(row.get("llm_call_id") or "").strip()
        if not call_id:
            canonical_missing_identity_count += 1
            continue
        canonical_identity_counts[call_id] = canonical_identity_counts.get(call_id, 0) + 1
    duplicate_canonical_call_ids = sorted(
        call_id for call_id, count in canonical_identity_counts.items() if count != 1
    )
    mirror_rows = [
        row
        for row in legacy_rows
        if not bool(row.get("response_reused"))
        and row.get("response_kind") != "external_ai_run"
    ]
    legacy_identity_counts: dict[str, int] = {}
    for row in mirror_rows:
        call_id = str(row.get("llm_call_id") or "").strip()
        if call_id:
            legacy_identity_counts[call_id] = legacy_identity_counts.get(call_id, 0) + 1
    duplicate_legacy_call_ids = sorted(
        call_id for call_id, count in legacy_identity_counts.items() if count != 1
    )
    canonical = latest_by_call(canonical_rows, "observed_at_ms")
    legacy = latest_by_call(mirror_rows, "timestamp_ms")
    canonical_ids = set(canonical)
    legacy_ids = set(legacy)
    mirror_expected_ids = {
        call_id
        for call_id, row in canonical.items()
        if runtime_mirror_expected_pricing(row.get("pricing_version"))
    }
    exact_parity_ids = {
        call_id
        for call_id, row in canonical.items()
        if runtime_mirror_exact_parity_pricing(row.get("pricing_version"))
    }
    joined_ids = mirror_expected_ids & legacy_ids
    exact_parity_joined_ids = exact_parity_ids & legacy_ids
    canonical_only = sorted(mirror_expected_ids - legacy_ids)
    legacy_only = sorted(legacy_ids - canonical_ids)
    parity_mismatches: list[dict[str, Any]] = []
    cohort = {"direct": 0, "queued": 0}
    for current in canonical.values():
        cohort["queued" if current.get("dispatch_job_id") else "direct"] += 1
    for call_id in sorted(exact_parity_joined_ids):
        current = canonical[call_id]
        mirror = legacy[call_id]
        if (current.get("dispatch_job_id") or None) != (
            mirror.get("dispatch_job_id") or None
        ):
            parity_mismatches.append(
                {
                    "llm_call_id": call_id,
                    "field": "dispatch_job_id",
                    "canonical": current.get("dispatch_job_id"),
                    "legacy": mirror.get("dispatch_job_id"),
                }
            )
        for field in (*INTEGER_PARITY_FIELDS, "cost_usd"):
            if not numbers_equal(field, current.get(field), mirror.get(field)):
                parity_mismatches.append(
                    {
                        "llm_call_id": call_id,
                        "field": field,
                        "canonical": current.get(field),
                        "legacy": mirror.get(field),
                    }
                )

    expected_attempts = sum(
        int(row.get("provider_attempt_count") or 0)
        for row in canonical.values()
    )
    captured_attempt_ids: set[str] = set()
    captured_attempt_counts: dict[str, int] = {}
    malformed_attempt_identities: list[dict[str, Any]] = []
    orphan_attempt_ids: list[str] = []
    duplicate_attempt_ids: list[str] = []
    attempt_call_fact_mismatches: list[dict[str, Any]] = []
    for row in attempts:
        attempt_id = str(row.get("provider_attempt_id") or "").strip()
        call_id = str(row.get("llm_call_id") or "").strip()
        try:
            attempt_index = int(row.get("provider_attempt_index") or 0)
        except (TypeError, ValueError):
            attempt_index = 0
        if call_id not in canonical_ids:
            orphan_attempt_ids.append(attempt_id or "<missing-provider-attempt-id>")
            continue
        declared_attempt_count = int(
            canonical[call_id].get("provider_attempt_count") or 0
        )
        expected_attempt_id = f"{call_id}:a{attempt_index}"
        if (
            attempt_index <= 0
            or attempt_index > declared_attempt_count
            or attempt_id != expected_attempt_id
        ):
            malformed_attempt_identities.append(
                {
                    "llm_call_id": call_id,
                    "provider_attempt_id": attempt_id,
                    "provider_attempt_index": attempt_index,
                    "declared_provider_attempt_count": declared_attempt_count,
                    "expected_provider_attempt_id": expected_attempt_id,
                }
            )
            continue
        if attempt_id in captured_attempt_ids:
            duplicate_attempt_ids.append(attempt_id)
            continue
        captured_attempt_ids.add(attempt_id)
        captured_attempt_counts[call_id] = captured_attempt_counts.get(call_id, 0) + 1
        attempt_state = str(row.get("attempt_terminal_state") or "")
        attempt_succeeded = attempt_state == "succeeded"
        if attempt_state not in {"succeeded", "failed", "cancelled", "timed_out"}:
            terminal_semantic_mismatches.append(
                {"dataset": "provider_attempts", "field": "attempt_terminal_state"}
            )
        for field in ("transport_success", "success"):
            if not isinstance(row.get(field), bool) or row.get(field) != attempt_succeeded:
                terminal_semantic_mismatches.append(
                    {"dataset": "provider_attempts", "field": field}
                )
        if attempt_index == declared_attempt_count:
            call = canonical[call_id]
            call_succeeded = call.get("call_terminal_state") == "succeeded"
            if call_succeeded != attempt_succeeded:
                terminal_semantic_mismatches.append(
                    {"dataset": "call_attempt", "field": "terminal_outcome"}
                )
            for field in (*ATTEMPT_INTEGER_PARITY_FIELDS, *ATTEMPT_COST_PARITY_FIELDS):
                if not numbers_equal(field, row.get(field), call.get(field)):
                    attempt_call_fact_mismatches.append(
                        {"llm_call_id": call_id, "field": field}
                    )
            for field in ("pricing_version", "cost_source"):
                if row.get(field) != call.get(field):
                    attempt_call_fact_mismatches.append(
                        {"llm_call_id": call_id, "field": field}
                    )
    attempt_gap_counts: dict[str, int] = {}
    malformed_attempt_gaps: list[dict[str, Any]] = []
    orphan_owned_gaps: list[dict[str, Any]] = []
    for row in gaps:
        owned_call_id = str(row.get("llm_call_id") or "").strip()
        if owned_call_id and owned_call_id not in canonical_ids:
            orphan_owned_gaps.append({"reason": str(row.get("gap_reason") or "")})
        if row.get("gap_reason") not in {
            ATTEMPT_GAP_REASON,
            TERMINAL_ATTEMPT_GAP_REASON,
        }:
            continue
        call_id = str(row.get("llm_call_id") or "").strip()
        try:
            missing_count = int(row.get("missing_record_count") or 0)
        except (TypeError, ValueError):
            missing_count = 0
        if not call_id or call_id not in canonical_ids or missing_count <= 0:
            malformed_attempt_gaps.append(
                {
                    "llm_call_id": call_id or "<missing-llm-call-id>",
                    "reason": str(row.get("gap_reason") or ""),
                    "missing_record_count": missing_count,
                }
            )
            continue
        attempt_gap_counts[call_id] = (
            attempt_gap_counts.get(call_id, 0) + missing_count
        )
    missing_attempts = sum(attempt_gap_counts.values())
    attempt_accounting_violations: list[dict[str, Any]] = []
    for call_id, row in canonical.items():
        expected = int(row.get("provider_attempt_count") or 0)
        captured = captured_attempt_counts.get(call_id, 0)
        unavailable = attempt_gap_counts.get(call_id, 0)
        if captured + unavailable != expected:
            attempt_accounting_violations.append(
                {
                    "llm_call_id": call_id,
                    "expected": expected,
                    "captured": captured,
                    "explicit_unavailable": unavailable,
                }
            )
    classified_missing_records = sum(
        int(row.get("missing_record_count") or 0)
        for row in gaps
        if row.get("gap_reason") not in UNCLASSIFIED_TRANSPORT_LAG_REASONS
    )
    known_missing_fact_revisions = sum(
        int(row.get("missing_record_count") or 0)
        for row in gaps
        if row.get("gap_reason") in KNOWN_MISSING_FACT_REASONS
    )
    unclassified_transport_events_lost = sum(
        int(row.get("missing_record_count") or 0)
        for row in gaps
        if row.get("gap_reason") in UNCLASSIFIED_TRANSPORT_LAG_REASONS
    )
    pricing_missing = sorted(
        call_id
        for call_id, row in canonical.items()
        if (row.get("call_terminal_state") == "succeeded" or row.get("cost_usd") is not None)
        and (
            not str(row.get("pricing_version") or "").strip()
            or str(row.get("cost_source") or "")
            not in {"provider", "computed", "estimated", "unknown"}
        )
    )
    dispatch_by_identity: dict[tuple[str, str], dict[str, Any]] = {}
    for row in dispatch_rows:
        if bool(row.get("response_reused")):
            continue
        if str(row.get("state") or "") not in {"completed", "failed", "tombstoned"}:
            continue
        job_id = str(row.get("job_id") or "").strip()
        call_id = str(row.get("llm_call_id") or "").strip()
        if not job_id or not call_id:
            continue
        identity = (job_id, call_id)
        existing = dispatch_by_identity.get(identity)
        row_time = int(row.get("completed_at_ms") or row.get("timestamp_ms") or 0)
        existing_time = int(
            (existing or {}).get("completed_at_ms")
            or (existing or {}).get("timestamp_ms")
            or 0
        )
        if existing is None or row_time >= existing_time:
            dispatch_by_identity[identity] = row
    queued_calls = [
        row for row in canonical.values() if str(row.get("dispatch_job_id") or "").strip()
    ]
    queued_dispatch_matches = 0
    queued_timing_missing: list[str] = []
    for row in queued_calls:
        call_id = str(row.get("llm_call_id") or "")
        dispatch = dispatch_by_identity.get((str(row.get("dispatch_job_id")), call_id))
        if dispatch is None:
            continue
        queued_dispatch_matches += 1
        if dispatch.get("wait_ms") is None or dispatch.get("execution_ms") is None:
            queued_timing_missing.append(call_id)
    queued_dispatch_join_rate = (
        queued_dispatch_matches / len(queued_calls) if queued_calls else 1.0
    )
    match_rate = (
        len(joined_ids) / len(mirror_expected_ids) if mirror_expected_ids else 1.0
    )
    return {
        "canonical_calls": len(canonical_ids),
        "canonical_missing_identity_count": canonical_missing_identity_count,
        "duplicate_canonical_call_ids": duplicate_canonical_call_ids[:100],
        "duplicate_canonical_call_count": len(duplicate_canonical_call_ids),
        "compatibility_mirror_expected_calls": len(mirror_expected_ids),
        "compatibility_exact_parity_calls": len(exact_parity_ids),
        "legacy_calls_with_identity": len(legacy_ids),
        "duplicate_legacy_call_ids": duplicate_legacy_call_ids[:100],
        "duplicate_legacy_call_count": len(duplicate_legacy_call_ids),
        "joined_calls": len(joined_ids),
        "canonical_to_legacy_match_rate": match_rate,
        "canonical_only_call_ids": canonical_only[:100],
        "canonical_only_count": len(canonical_only),
        "legacy_only_call_ids": legacy_only[:100],
        "legacy_only_count": len(legacy_only),
        "parity_mismatches": parity_mismatches[:100],
        "parity_mismatch_count": len(parity_mismatches),
        "cohorts": cohort,
        "expected_provider_attempts": expected_attempts,
        "captured_terminal_attempts": len(captured_attempt_ids),
        "explicit_unavailable_attempts": missing_attempts,
        "attempt_accounting_complete": (
            not attempt_accounting_violations and not malformed_attempt_gaps
        ),
        "attempt_accounting_violations": attempt_accounting_violations[:100],
        "attempt_accounting_violation_count": len(attempt_accounting_violations),
        "malformed_attempt_gaps": malformed_attempt_gaps[:100],
        "malformed_attempt_gap_count": len(malformed_attempt_gaps),
        "malformed_attempt_identities": malformed_attempt_identities[:100],
        "malformed_attempt_identity_count": len(malformed_attempt_identities),
        "orphan_attempt_ids": orphan_attempt_ids[:100],
        "orphan_attempt_count": len(orphan_attempt_ids),
        "duplicate_attempt_ids": duplicate_attempt_ids[:100],
        "duplicate_attempt_count": len(duplicate_attempt_ids),
        "attempt_call_fact_mismatches": attempt_call_fact_mismatches[:100],
        "attempt_call_fact_mismatch_count": len(attempt_call_fact_mismatches),
        "orphan_owned_gaps": orphan_owned_gaps[:100],
        "orphan_owned_gap_count": len(orphan_owned_gaps),
        "embedded_scope_mismatches": scope_mismatches[:100],
        "embedded_scope_mismatch_count": len(scope_mismatches),
        "categorical_content_violations": category_violations[:100],
        "categorical_content_violation_count": len(category_violations),
        "noncanonical_identity_fields": noncanonical_identity_fields[:100],
        "noncanonical_identity_field_count": len(noncanonical_identity_fields),
        "terminal_semantic_mismatches": terminal_semantic_mismatches[:100],
        "terminal_semantic_mismatch_count": len(terminal_semantic_mismatches),
        "pricing_metadata_missing_call_ids": pricing_missing[:100],
        "pricing_metadata_missing_count": len(pricing_missing),
        "queued_calls": len(queued_calls),
        "queued_dispatch_matches": queued_dispatch_matches,
        "queued_dispatch_join_rate": queued_dispatch_join_rate,
        "queued_timing_missing_call_ids": queued_timing_missing[:100],
        "queued_timing_missing_count": len(queued_timing_missing),
        "gap_missing_record_count": classified_missing_records,
        "known_missing_fact_revisions": known_missing_fact_revisions,
        "unclassified_transport_events_lost": unclassified_transport_events_lost,
        "gap_reasons": sorted(
            {
                str(row.get("gap_reason"))
                for row in gaps
                if row.get("gap_reason")
            }
        ),
    }


def read_live_rows(
    runtime_root: Path,
    principal: str,
    workspace: str,
    from_ms: int,
    to_ms: int,
) -> tuple[
    list[dict[str, Any]],
    list[dict[str, Any]],
    list[dict[str, Any]],
    list[dict[str, Any]],
    list[dict[str, Any]],
]:
    try:
        import duckdb  # type: ignore
    except ImportError as error:
        raise RuntimeError("duckdb Python package is required for Phase 2F") from error

    validate_scope_component(principal, "principal")
    validate_scope_component(workspace, "workspace")
    analytics = runtime_root / "scopes" / principal / workspace / "analytics"
    ensure_no_symlink_components(runtime_root, analytics)
    call_root = analytics / "llm_calls"
    attempt_root = analytics / "llm_provider_attempts"
    gap_root = analytics / "llm_capture_gaps"
    dispatch_root = analytics / "llm_dispatch"
    canonical_files = safe_parquet_files(call_root, "dt=*/part-call_fact-*.parquet")
    legacy_files = safe_parquet_files(call_root, "dt=*/batch_*.parquet")
    attempt_files = safe_parquet_files(attempt_root, "dt=*/part-provider_attempt-*.parquet")
    gap_files = safe_parquet_files(gap_root, "dt=*/part-capture_gap-*.parquet")
    dispatch_files = safe_parquet_files(dispatch_root, "dt=*/batch_*.parquet")
    connection = duckdb.connect()

    canonical_rows: list[dict[str, Any]] = []
    if canonical_files:
        columns = parquet_columns(connection, canonical_files)
        canonical_rows = fetch_dicts(
            connection,
            f"SELECT {typed_projection(CALL_FIELDS, columns)} FROM read_parquet({sql_files(canonical_files)}, union_by_name=true) "
            f"WHERE record_kind='call_fact' AND record_revision=2 AND observed_at_ms >= {from_ms} AND observed_at_ms < {to_ms}",
        )
    legacy_rows: list[dict[str, Any]] = []
    if legacy_files:
        columns = parquet_columns(connection, legacy_files)
        legacy_fallbacks = {
            "provider_attempt_count": (
                "COALESCE(attempt, 1)" if "attempt" in columns else "1"
            ),
            "response_reused": "false",
        }
        legacy_projection = typed_projection(
            LEGACY_FIELDS, columns, fallbacks=legacy_fallbacks
        )
        legacy_rows = fetch_dicts(
            connection,
            f"SELECT {legacy_projection} FROM read_parquet({sql_files(legacy_files)}, union_by_name=true) "
            f"WHERE timestamp_ms >= {from_ms} AND timestamp_ms < {to_ms}",
        )
    attempts: list[dict[str, Any]] = []
    if attempt_files:
        columns = parquet_columns(connection, attempt_files)
        attempt_fields = (
            "principal",
            "workspace",
            "provider_attempt_id",
            "provider_attempt_index",
            "llm_call_id",
            "error_class",
            "error_code",
            "finish_reason",
            "training_exclusion_reason",
            "attempt_terminal_state",
            "transport_success",
            "success",
            "input_tokens",
            "output_tokens",
            "reasoning_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "audio_input_tokens",
            "audio_output_tokens",
            "audio_cached_tokens",
            "total_tokens",
            "pricing_version",
            "cost_source",
            "input_cost_usd",
            "output_cost_usd",
            "reasoning_cost_usd",
            "cache_cost_usd",
            "cost_usd",
            "observed_at_ms",
        )
        attempts = fetch_dicts(
            connection,
            f"SELECT {typed_projection(attempt_fields, columns)} FROM read_parquet({sql_files(attempt_files)}, union_by_name=true) "
            f"WHERE record_kind='provider_attempt' AND record_revision=3 AND observed_at_ms >= {from_ms} AND observed_at_ms < {to_ms}",
        )
    gaps: list[dict[str, Any]] = []
    if gap_files:
        columns = parquet_columns(connection, gap_files)
        gap_fields = (
            "principal",
            "workspace",
            "llm_call_id",
            "gap_reason",
            "missing_record_count",
            "observed_at_ms",
        )
        gaps = fetch_dicts(
            connection,
            f"SELECT {typed_projection(gap_fields, columns)} FROM read_parquet({sql_files(gap_files)}, union_by_name=true) "
            f"WHERE record_kind='capture_gap' AND observed_at_ms >= {from_ms} AND observed_at_ms < {to_ms}",
        )
    # Broadcast lag cannot be attributed to a tenant because the skipped
    # envelopes are unavailable. Activation stores that process-level
    # diagnostic only in anonymous/default; include it in every strict audit
    # without exposing any content or treating it as a scoped LLM fact.
    if (principal, workspace) != ("anonymous", "default"):
        diagnostic_root = (
            runtime_root
            / "scopes"
            / "anonymous"
            / "default"
            / "analytics"
            / "llm_capture_gaps"
        )
        ensure_no_symlink_components(runtime_root, diagnostic_root)
        diagnostic_files = safe_parquet_files(
            diagnostic_root,
            "dt=*/part-capture_gap-*.parquet",
        )
        if diagnostic_files:
            columns = parquet_columns(connection, diagnostic_files)
            gap_fields = (
                "principal",
                "workspace",
                "llm_call_id",
                "gap_reason",
                "missing_record_count",
                "observed_at_ms",
            )
            gaps.extend(
                fetch_dicts(
                    connection,
                    f"SELECT {typed_projection(gap_fields, columns)} FROM read_parquet({sql_files(diagnostic_files)}, union_by_name=true) "
                    f"WHERE record_kind='capture_gap' AND gap_reason IN ({', '.join(sql_string(reason) for reason in sorted(UNCLASSIFIED_TRANSPORT_LAG_REASONS))}) "
                    f"AND observed_at_ms >= {from_ms} AND observed_at_ms < {to_ms}",
                )
            )
    dispatch_rows: list[dict[str, Any]] = []
    if dispatch_files:
        columns = parquet_columns(connection, dispatch_files)
        dispatch_fallbacks = {
            "response_reused": "false",
            "completed_at_ms": (
                "timestamp_ms" if "timestamp_ms" in columns else "NULL::BIGINT"
            ),
            "timestamp_ms": (
                "completed_at_ms"
                if "completed_at_ms" in columns
                else "NULL::BIGINT"
            ),
        }
        dispatch_projection = typed_projection(
            DISPATCH_FIELDS, columns, fallbacks=dispatch_fallbacks
        )
        time_expression = None
        if {"completed_at_ms", "timestamp_ms"}.issubset(columns):
            time_expression = "COALESCE(completed_at_ms, timestamp_ms)"
        elif "completed_at_ms" in columns:
            time_expression = "completed_at_ms"
        elif "timestamp_ms" in columns:
            time_expression = "timestamp_ms"
        if time_expression is not None:
            dispatch_rows = fetch_dicts(
                connection,
                f"SELECT {dispatch_projection} FROM read_parquet({sql_files(dispatch_files)}, union_by_name=true) "
                f"WHERE {time_expression} >= {from_ms} AND {time_expression} < {to_ms}",
            )
    connection.close()
    return canonical_rows, legacy_rows, attempts, gaps, dispatch_rows


def read_overview_api(
    api_base: str,
    principal: str,
    workspace: str,
    from_ms: int,
    to_ms: int,
) -> dict[str, Any]:
    query = urllib.parse.urlencode({"from_ms": from_ms, "to_ms": to_ms})
    request = urllib.request.Request(
        f"{api_base.rstrip('/')}/api/magician/v2/analytics/llm/overview?{query}"
    )
    bearer = os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
    if bearer:
        request.add_header("Authorization", f"Bearer {bearer}")
    with urllib.request.urlopen(request, timeout=30) as response:
        if response.status != 200:
            raise RuntimeError(f"overview API returned HTTP {response.status}")
        return json.loads(response.read().decode("utf-8"))


def evaluate(
    reconciliation: dict[str, Any], api: dict[str, Any] | None, api_error: str | None
) -> tuple[str, list[str]]:
    failures: list[str] = []
    if reconciliation["canonical_missing_identity_count"]:
        failures.append("one or more canonical terminal calls lack stable identity")
    if reconciliation["duplicate_canonical_call_count"]:
        failures.append("one or more canonical terminal-call identities are duplicated")
    if reconciliation["duplicate_legacy_call_count"]:
        failures.append("one or more compatibility mirror call identities are duplicated")
    if reconciliation["legacy_only_count"]:
        failures.append(
            "one or more compatibility response rows lack a canonical call lifecycle"
        )
    if not reconciliation["attempt_accounting_complete"]:
        failures.append(
            "captured attempts plus call-owned explicit gaps do not reconcile for every call"
        )
    if reconciliation["malformed_attempt_identity_count"]:
        failures.append("one or more provider attempts violate stable call:index identity")
    if reconciliation["orphan_attempt_count"]:
        failures.append("one or more provider attempts lack a canonical terminal call")
    if reconciliation["duplicate_attempt_count"]:
        failures.append("one or more terminal provider-attempt identities are duplicated")
    if reconciliation["attempt_call_fact_mismatch_count"]:
        failures.append(
            "one or more final provider attempts disagree with their logical call usage or pricing"
        )
    if reconciliation["orphan_owned_gap_count"]:
        failures.append("one or more call-owned capture gaps lack a canonical call")
    if reconciliation["embedded_scope_mismatch_count"]:
        failures.append("one or more raw facts disagree with the requested embedded scope")
    if reconciliation["categorical_content_violation_count"]:
        failures.append(
            "one or more content-free categorical facts contain prose, invalid characters, or oversized values"
        )
    if reconciliation["noncanonical_identity_field_count"]:
        failures.append(
            "one or more stable identity fields are blank or contain boundary whitespace"
        )
    if reconciliation["terminal_semantic_mismatch_count"]:
        failures.append(
            "one or more terminal state, success flag, or final-attempt transport outcomes disagree"
        )
    if reconciliation["unclassified_transport_events_lost"]:
        failures.append(
            "unclassified transport loss prevents proving complete LLM capture"
        )
    if reconciliation["canonical_calls"] == 0:
        if failures:
            return "failed", failures
        return "no_canonical_data", ["no canonical completed calls exist in the selected range"]
    if reconciliation["canonical_to_legacy_match_rate"] < 0.99:
        failures.append("canonical-to-compatibility call match rate is below 99%")
    if reconciliation["parity_mismatch_count"]:
        failures.append("tokens, cost, or latency differ for matched call ids")
    if reconciliation["pricing_metadata_missing_count"]:
        failures.append("one or more canonical calls lack pricing source/version")
    if reconciliation["queued_dispatch_join_rate"] < 0.99:
        failures.append("queued canonical-to-dispatch timing join rate is below 99%")
    if reconciliation["queued_timing_missing_count"]:
        failures.append("one or more joined queued calls lack wait or execution timing")
    if api_error:
        failures.append(f"governed overview API failed: {api_error}")
    elif api is not None:
        scope = api.get("scope") or {}
        if scope != {
            "principal": api.get("data", {}).get("principal"),
            "workspace": api.get("data", {}).get("workspace"),
        }:
            failures.append("overview envelope scope and data scope disagree")
        if int(api.get("data", {}).get("logical_calls") or 0) < int(
            reconciliation["canonical_calls"]
        ):
            failures.append("overview API reports fewer logical calls than canonical materialization")
        if int(api.get("data", {}).get("capture_gaps") or 0) != int(
            reconciliation["gap_missing_record_count"]
        ):
            failures.append("overview API classified-gap count disagrees with canonical facts")
        if int(api.get("data", {}).get("known_missing_fact_revisions") or 0) != int(
            reconciliation["known_missing_fact_revisions"]
        ):
            failures.append(
                "overview API known-missing fact count disagrees with canonical facts"
            )
        api_scope_is_diagnostic = api.get("data", {}).get("principal") == "anonymous" and api.get(
            "data", {}
        ).get("workspace") == "default"
        if api_scope_is_diagnostic and int(
            api.get("data", {}).get("unclassified_transport_events_lost") or 0
        ) != int(reconciliation["unclassified_transport_events_lost"]):
            failures.append(
                "overview API unclassified transport-loss count disagrees with canonical facts"
            )
        if reconciliation["queued_calls"] and (
            api.get("data", {}).get("average_queue_wait_ms") is None
            or api.get("data", {}).get("average_provider_execution_ms") is None
        ):
            failures.append("overview API omitted governed queued timing enrichment")
    return ("failed" if failures else "passed"), failures


def render_html(report: dict[str, Any]) -> str:
    reconciliation = report["reconciliation"]
    failures = "".join(f"<li>{html.escape(item)}</li>" for item in report["failures"])
    mismatch_rows = "".join(
        "<tr>"
        + "".join(f"<td>{html.escape(str(row.get(key)))}</td>" for key in ("llm_call_id", "field", "canonical", "legacy"))
        + "</tr>"
        for row in reconciliation["parity_mismatches"]
    ) or '<tr><td colspan="4">No mismatches</td></tr>'
    status_class = "ok" if report["status"] == "passed" else "bad"
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>LLM observability Phase 2F activation audit</title><style>
body{{font-family:Inter,system-ui,sans-serif;background:#0b1020;color:#e8ecf7;margin:0;padding:32px}}main{{max-width:1120px;margin:auto}}
.card{{background:#121a2e;border:1px solid #273452;border-radius:14px;padding:18px;margin:16px 0}}.status{{display:inline-block;padding:6px 11px;border-radius:999px}}.ok{{background:#183d32;color:#8af0c4}}.bad{{background:#4a202a;color:#ffb3c1}}
.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(190px,1fr));gap:12px}}.metric{{background:#0e1628;border-radius:10px;padding:14px}}.metric b{{display:block;font-size:1.45rem}}table{{width:100%;border-collapse:collapse}}th,td{{text-align:left;padding:8px;border-bottom:1px solid #273452}}code{{color:#9dd9ff}}
</style></head><body><main><h1>LLM observability Phase 2F</h1>
<section class="card"><span class="status {status_class}">{html.escape(report['status'])}</span><p>Generated {html.escape(report['generated_at'])}. Canonical journal activation, compatibility reconciliation, attempt accounting, pricing, timing, and governed API audit.</p><ul>{failures}</ul></section>
<section class="card grid"><div class="metric"><b>{reconciliation['canonical_calls']:,}</b>canonical calls</div><div class="metric"><b>{reconciliation['canonical_to_legacy_match_rate']*100:.2f}%</b>legacy mirror match</div><div class="metric"><b>{reconciliation['queued_dispatch_join_rate']*100:.2f}%</b>queued timing joins</div><div class="metric"><b>{reconciliation['captured_terminal_attempts']:,} + {reconciliation['explicit_unavailable_attempts']:,}</b>captured + explicit attempts</div><div class="metric"><b>{reconciliation['embedded_scope_mismatch_count']:,}</b>scope mismatches</div><div class="metric"><b>{reconciliation['categorical_content_violation_count']:,}</b>categorical content violations</div><div class="metric"><b>{reconciliation['canonical_missing_identity_count'] + reconciliation['duplicate_canonical_call_count'] + reconciliation['duplicate_legacy_call_count'] + reconciliation['noncanonical_identity_field_count']:,}</b>call/field identity violations</div><div class="metric"><b>{reconciliation['malformed_attempt_identity_count'] + reconciliation['orphan_attempt_count'] + reconciliation['duplicate_attempt_count'] + reconciliation['attempt_call_fact_mismatch_count']:,}</b>attempt identity/fact violations</div><div class="metric"><b>{reconciliation['terminal_semantic_mismatch_count']:,}</b>terminal semantic violations</div><div class="metric"><b>{reconciliation['orphan_owned_gap_count']:,}</b>orphan call-owned gaps</div><div class="metric"><b>{reconciliation['known_missing_fact_revisions']:,}</b>known missing fact revisions</div><div class="metric"><b>{reconciliation['gap_missing_record_count']:,}</b>classified gap units</div><div class="metric"><b>{reconciliation['unclassified_transport_events_lost']:,}</b>unclassified transport envelopes lost</div></section>
<section class="card"><h2>Direct and queued cohorts</h2><p>Direct: {reconciliation['cohorts']['direct']:,} · queued: {reconciliation['cohorts']['queued']:,}. Expected attempts: {reconciliation['expected_provider_attempts']:,}. Accounting complete: <code>{reconciliation['attempt_accounting_complete']}</code>.</p><p>Gap reasons: <code>{html.escape(json.dumps(reconciliation['gap_reasons']))}</code></p></section>
<section class="card"><h2>Parity mismatches</h2><table><thead><tr><th>Call</th><th>Field</th><th>Canonical</th><th>Legacy</th></tr></thead><tbody>{mismatch_rows}</tbody></table></section>
<section class="card"><h2>Privacy</h2><p>Content fields read: none. Only stable identity, lifecycle, token, cost, timing, pricing, scope, and gap facts were inspected.</p></section>
</main></body></html>"""


def self_test() -> None:
    canonical = [
        {
            "llm_call_id": "call-1",
            "dispatch_job_id": None,
            "provider_attempt_count": 2,
            "input_tokens": 10,
            "output_tokens": 2,
            "reasoning_tokens": 1,
            "cache_read_tokens": 4,
            "cache_creation_tokens": 0,
            "cost_usd": 0.01,
            "latency_ms": 100,
            "call_terminal_state": "succeeded",
            "transport_success": True,
            "success": True,
            "pricing_version": "pricing-row-v1:" + "a" * 64,
            "cost_source": "computed",
            "observed_at_ms": 10,
        }
    ]
    legacy = [
        {
            **canonical[0],
            "timestamp_ms": 10,
            "response_reused": False,
            "response_kind": "text",
        }
    ]
    attempts = [
        {
            "provider_attempt_id": "call-1:a2",
            "provider_attempt_index": 2,
            "llm_call_id": "call-1",
            "attempt_terminal_state": "succeeded",
            "transport_success": True,
            "success": True,
            **{
                field: canonical[0].get(field)
                for field in (
                    *ATTEMPT_INTEGER_PARITY_FIELDS,
                    *ATTEMPT_COST_PARITY_FIELDS,
                    "pricing_version",
                    "cost_source",
                )
            },
        }
    ]
    gaps = [
        {
            "llm_call_id": "call-1",
            "gap_reason": ATTEMPT_GAP_REASON,
            "missing_record_count": 1,
        }
    ]
    dispatch = [
        {
            "job_id": "job-1",
            "llm_call_id": "call-1",
            "wait_ms": 10,
            "execution_ms": 90,
            "state": "completed",
            "completed_at_ms": 10,
            "response_reused": False,
        }
    ]
    canonical[0]["dispatch_job_id"] = "job-1"
    legacy[0]["dispatch_job_id"] = "job-1"
    result = reconcile(canonical, legacy, attempts, gaps, dispatch)
    assert result["canonical_to_legacy_match_rate"] == 1.0
    assert result["parity_mismatch_count"] == 0
    assert result["attempt_accounting_complete"]
    legacy[0]["input_tokens"] = 11
    assert reconcile(canonical, legacy, attempts, gaps, dispatch)["parity_mismatch_count"] == 1
    legacy[0]["input_tokens"] = canonical[0]["input_tokens"]
    legacy[0]["dispatch_job_id"] = "unexpected-job"
    assert reconcile(canonical, legacy, attempts, gaps, dispatch)["parity_mismatch_count"] == 1
    legacy[0]["dispatch_job_id"] = canonical[0]["dispatch_job_id"]
    assert not reconcile(canonical, legacy, attempts, [], dispatch)["attempt_accounting_complete"]
    assert reconcile(canonical, legacy, attempts, gaps, dispatch)["queued_dispatch_join_rate"] == 1.0
    assert reconcile(canonical, legacy, attempts, gaps, [])["queued_dispatch_join_rate"] == 0.0
    canonical[0]["pricing_version"] = "runtime-pricing-miss@call-time"
    assert reconcile(canonical, legacy, attempts, gaps, dispatch)[
        "canonical_to_legacy_match_rate"
    ] == 1.0
    canonical[0]["pricing_version"] = "runtime-pricing-invalid-value@call-time"
    assert reconcile(canonical, legacy, attempts, gaps, dispatch)[
        "canonical_to_legacy_match_rate"
    ] == 1.0
    canonical[0]["pricing_version"] = "pricing-row-v1:" + "a" * 64
    canonical[0]["pricing_version"] = "pricing-row-v1:not-a-fingerprint"
    malformed_pricing = reconcile(canonical, legacy, attempts, gaps, dispatch)
    assert malformed_pricing["categorical_content_violation_count"] == 1
    assert any(
        "categorical facts" in failure
        for failure in evaluate(malformed_pricing, None, None)[1]
    )
    canonical[0]["pricing_version"] = "pricing-row-v1:" + "a" * 64
    unreported = {
        **canonical[0],
        "llm_call_id": "call-unreported",
        "dispatch_job_id": None,
        "provider_attempt_count": 1,
        "input_tokens": None,
        "output_tokens": None,
        "reasoning_tokens": None,
        "cache_read_tokens": None,
        "cache_creation_tokens": None,
        "cost_usd": None,
        "pricing_version": "provider-usage-unreported@call-time",
        "cost_source": "unknown",
    }
    unreported_legacy = {
        **unreported,
        "input_tokens": 0,
        "output_tokens": 0,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_creation_tokens": 0,
        "cost_usd": 0.0,
        "timestamp_ms": 10,
        "response_reused": False,
        "response_kind": "text",
    }
    unreported_result = reconcile(
        [unreported],
        [unreported_legacy],
        [
            {
                "provider_attempt_id": "call-unreported:a1",
                "provider_attempt_index": 1,
                "llm_call_id": "call-unreported",
                "attempt_terminal_state": "succeeded",
                "transport_success": True,
                "success": True,
                **{
                    field: unreported.get(field)
                    for field in (
                        *ATTEMPT_INTEGER_PARITY_FIELDS,
                        *ATTEMPT_COST_PARITY_FIELDS,
                        "pricing_version",
                        "cost_source",
                    )
                },
            }
        ],
        [],
        [],
    )
    assert unreported_result["canonical_to_legacy_match_rate"] == 1.0
    assert unreported_result["compatibility_exact_parity_calls"] == 0
    assert unreported_result["parity_mismatch_count"] == 0
    orphan_result = reconcile(
        [],
        [
            {
                "llm_call_id": "orphan-legacy",
                "timestamp_ms": 10,
                "response_reused": False,
                "response_kind": "text",
            }
        ],
        [],
        [],
        [],
    )
    assert orphan_result["legacy_only_count"] == 1
    assert any(
        "lack a canonical call lifecycle" in failure
        for failure in evaluate(orphan_result, None, None)[1]
    )
    excluded_external = reconcile(
        [],
        [
            {
                "llm_call_id": "opaque-external",
                "timestamp_ms": 10,
                "response_reused": False,
                "response_kind": "external_ai_run",
            }
        ],
        [],
        [],
        [],
    )
    assert excluded_external["legacy_only_count"] == 0
    malformed_identity = reconcile(
        canonical,
        legacy,
        [
            {
                "provider_attempt_id": "call-1:a1",
                "provider_attempt_index": 2,
                "llm_call_id": "call-1",
            }
        ],
        gaps,
        dispatch,
    )
    assert malformed_identity["malformed_attempt_identity_count"] == 1
    assert any(
        "stable call:index identity" in failure
        for failure in evaluate(malformed_identity, None, None)[1]
    )
    drifted_attempt = dict(attempts[0])
    drifted_attempt["input_tokens"] = int(drifted_attempt["input_tokens"] or 0) + 1
    attempt_fact_drift = reconcile(
        canonical, legacy, [drifted_attempt], gaps, dispatch
    )
    assert attempt_fact_drift["attempt_call_fact_mismatch_count"] == 1
    assert any(
        "logical call usage or pricing" in failure
        for failure in evaluate(attempt_fact_drift, None, None)[1]
    )
    failed_attempt = {
        **attempts[0],
        "attempt_terminal_state": "failed",
        "transport_success": False,
        "success": False,
    }
    terminal_drift = reconcile(canonical, legacy, [failed_attempt], gaps, dispatch)
    assert terminal_drift["terminal_semantic_mismatch_count"] == 1
    assert any(
        "terminal state" in failure
        for failure in evaluate(terminal_drift, None, None)[1]
    )
    bad_success_flag = reconcile(
        [{**canonical[0], "transport_success": False}],
        legacy,
        attempts,
        gaps,
        dispatch,
    )
    assert bad_success_flag["terminal_semantic_mismatch_count"] == 1
    orphan_gap = reconcile(
        canonical,
        legacy,
        attempts,
        [
            *gaps,
            {
                "llm_call_id": "absent-call",
                "gap_reason": "invalid_provider_usage",
                "missing_record_count": 1,
            },
        ],
        dispatch,
    )
    assert orphan_gap["orphan_owned_gap_count"] == 1
    assert any(
        "call-owned capture gaps" in failure
        for failure in evaluate(orphan_gap, None, None)[1]
    )
    padded_identity = reconcile(
        [{**canonical[0], "llm_call_id": " call-1"}],
        legacy,
        attempts,
        gaps,
        dispatch,
    )
    assert padded_identity["noncanonical_identity_field_count"] == 1
    assert any(
        "boundary whitespace" in failure
        for failure in evaluate(padded_identity, None, None)[1]
    )
    # Global attempt totals can match while gaps are assigned to the wrong
    # call. Per-call ownership must expose both the deficit and the surplus.
    second_call = {
        **canonical[0],
        "llm_call_id": "call-2",
        "dispatch_job_id": None,
        "provider_attempt_count": 1,
    }
    wrong_call_gap = [
        {
            "llm_call_id": "call-2",
            "gap_reason": ATTEMPT_GAP_REASON,
            "missing_record_count": 1,
        }
    ]
    globally_balanced_but_misattributed = reconcile(
        [canonical[0], second_call],
        [],
        [
            {
                "provider_attempt_id": "call-1:a2",
                "provider_attempt_index": 2,
                "llm_call_id": "call-1",
                "attempt_terminal_state": "succeeded",
                "transport_success": True,
                "success": True,
                **{
                    field: canonical[0].get(field)
                    for field in (
                        *ATTEMPT_INTEGER_PARITY_FIELDS,
                        *ATTEMPT_COST_PARITY_FIELDS,
                        "pricing_version",
                        "cost_source",
                    )
                },
            },
            {
                "provider_attempt_id": "call-2:a1",
                "provider_attempt_index": 1,
                "llm_call_id": "call-2",
                "attempt_terminal_state": "succeeded",
                "transport_success": True,
                "success": True,
                **{
                    field: second_call.get(field)
                    for field in (
                        *ATTEMPT_INTEGER_PARITY_FIELDS,
                        *ATTEMPT_COST_PARITY_FIELDS,
                        "pricing_version",
                        "cost_source",
                    )
                },
            },
        ],
        wrong_call_gap,
        [],
    )
    assert globally_balanced_but_misattributed["expected_provider_attempts"] == 3
    assert globally_balanced_but_misattributed["captured_terminal_attempts"] == 2
    assert globally_balanced_but_misattributed["explicit_unavailable_attempts"] == 1
    assert not globally_balanced_but_misattributed["attempt_accounting_complete"]
    assert globally_balanced_but_misattributed["attempt_accounting_violation_count"] == 2
    scoped_canonical = [
        {**canonical[0], "principal": "wrong", "workspace": "default"}
    ]
    scoped_result = reconcile(
        scoped_canonical,
        legacy,
        attempts,
        gaps,
        dispatch,
        expected_principal="anonymous",
        expected_workspace="default",
    )
    assert scoped_result["embedded_scope_mismatch_count"] == 5
    assert any(
        "embedded scope" in failure
        for failure in evaluate(scoped_result, None, None)[1]
    )
    duplicate_call_result = reconcile(
        [canonical[0], dict(canonical[0])],
        [legacy[0], dict(legacy[0])],
        attempts,
        gaps,
        dispatch,
    )
    assert duplicate_call_result["duplicate_canonical_call_count"] == 1
    assert duplicate_call_result["duplicate_legacy_call_count"] == 1
    assert any(
        "terminal-call identities are duplicated" in failure
        for failure in evaluate(duplicate_call_result, None, None)[1]
    )
    prose_category = reconcile(
        [{**canonical[0], "response_kind": "private user text"}],
        legacy,
        attempts,
        gaps,
        dispatch,
    )
    assert prose_category["categorical_content_violation_count"] == 1
    assert prose_category["categorical_content_violations"] == [
        {"dataset": "canonical_calls", "field": "response_kind"}
    ]
    assert any(
        "content-free categorical facts" in failure
        for failure in evaluate(prose_category, None, None)[1]
    )
    unclassified = [
        {
            "gap_reason": "runtime_transport_events_unclassified_due_broadcast_lag",
            "missing_record_count": 4,
        }
    ]
    loss_result = reconcile(canonical, legacy, attempts, gaps + unclassified, dispatch)
    assert loss_result["gap_missing_record_count"] == 1
    assert loss_result["known_missing_fact_revisions"] == 1
    assert loss_result["unclassified_transport_events_lost"] == 4
    _, loss_failures = evaluate(loss_result, None, "self-test API intentionally absent")
    assert any("unclassified transport loss" in failure for failure in loss_failures)
    old_projection = typed_projection(
        LEGACY_FIELDS,
        {"timestamp_ms", "attempt"},
        fallbacks={
            "provider_attempt_count": "COALESCE(attempt, 1)",
            "response_reused": "false",
        },
    )
    assert 'CAST(NULL AS VARCHAR) AS "llm_call_id"' in old_projection
    assert 'COALESCE(attempt, 1) AS "provider_attempt_count"' in old_projection
    for field in FORBIDDEN_CONTENT_FIELDS:
        assert (
            field not in CALL_FIELDS
            and field not in LEGACY_FIELDS
            and field not in DISPATCH_FIELDS
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--runtime-root",
        type=Path,
        default=Path(os.environ.get("MAGICIAN_ROOT_DIR", "~/MagicianNotes")).expanduser(),
    )
    parser.add_argument("--principal", default="anonymous")
    parser.add_argument("--workspace", default="default")
    parser.add_argument("--api-base", default=os.environ.get("MAGICIAN_URL", "http://127.0.0.1:3002"))
    parser.add_argument("--window-hours", type=float, default=24.0)
    parser.add_argument("--settle-seconds", type=float, default=float(os.environ.get("LLM_PHASE2F_SETTLE_SECONDS", "35")))
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("coverage/evals/llm-observability-phase2f/latest"),
    )
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("Phase 2F evaluator self-test passed")
        return 0
    if args.window_hours <= 0 or args.window_hours > 31 * 24:
        parser.error("--window-hours must be in (0, 744]")
    if args.settle_seconds < 0 or args.settle_seconds > 120:
        parser.error("--settle-seconds must be in [0, 120]")
    if args.dry_run:
        print(
            json.dumps(
                {
                    "runtime_root": str(args.runtime_root),
                    "scope": f"{args.principal}/{args.workspace}",
                    "api_base": args.api_base,
                    "settle_seconds": args.settle_seconds,
                    "content_fields_read": [],
                },
                indent=2,
            )
        )
        return 0

    if args.settle_seconds:
        print(f"Waiting {args.settle_seconds:g}s for the compatibility mirror flush boundary")
        time.sleep(args.settle_seconds)
    to_ms = int(time.time() * 1000)
    from_ms = to_ms - int(args.window_hours * 60 * 60 * 1000)
    try:
        canonical, legacy, attempts, gaps, dispatch = read_live_rows(
            args.runtime_root,
            args.principal,
            args.workspace,
            from_ms,
            to_ms,
        )
    except Exception as error:
        print(f"Phase 2F data read failed: {error}", file=sys.stderr)
        return 2
    reconciliation = reconcile(
        canonical,
        legacy,
        attempts,
        gaps,
        dispatch,
        expected_principal=args.principal,
        expected_workspace=args.workspace,
    )
    api: dict[str, Any] | None = None
    api_error: str | None = None
    try:
        api = read_overview_api(
            args.api_base,
            args.principal,
            args.workspace,
            from_ms,
            to_ms,
        )
    except Exception as error:
        api_error = str(error)
    status, failures = evaluate(reconciliation, api, api_error)
    report = {
        "schema_version": 1,
        "evaluation": "llm-observability-phase2f-activation",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "status": status,
        "runtime_root": str(args.runtime_root),
        "scope": {"principal": args.principal, "workspace": args.workspace},
        "range": {"from_ms": from_ms, "to_ms": to_ms},
        "reconciliation": reconciliation,
        "overview_api": api,
        "overview_api_error": api_error,
        "failures": failures,
        "privacy": {"content_fields_read": [], "raw_payloads_written": False},
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.output_dir / "report.json"
    html_path = args.output_dir / "report.html"
    json_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    html_path.write_text(render_html(report), encoding="utf-8")
    print(
        f"LLM observability Phase 2F: {status}; canonical={reconciliation['canonical_calls']}; "
        f"mirror_match={reconciliation['canonical_to_legacy_match_rate']*100:.2f}%; "
        f"attempt_accounting={reconciliation['attempt_accounting_complete']}"
    )
    print(f"Report: {html_path.resolve().as_uri()}")
    return 1 if args.strict and status != "passed" else 0


if __name__ == "__main__":
    raise SystemExit(main())

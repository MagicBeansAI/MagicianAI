#!/usr/bin/env python3
"""Content-free Phase 1 audit for stable LLM call/dispatch correlation."""

from __future__ import annotations

import argparse
import html
import json
import os
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable


CALL_FIELDS = (
    "schema_version", "fact_schema_version", "record_revision", "lifecycle_phase",
    "principal", "workspace", "trace_id", "llm_call_id",
    "provider_attempt_id", "provider_attempt_count", "dispatch_job_id",
    "parent_call_id", "parent_relation", "retry_group_id", "route_decision_id",
    "task_id", "root_execution_id", "execution_id", "plan_id", "step_id", "step_index",
    "iteration_id", "chat_session_id", "chat_turn_id", "user_message_id",
    "scope_resolution", "workload_class", "call_role",
    "response_reused",
)
DISPATCH_FIELDS = (
    "principal", "workspace", "trace_id", "llm_call_id", "provider_attempt_id",
    "provider_attempt_count", "job_id", "parent_call_id", "retry_group_id",
    "route_decision_id", "parent_relation", "task_id", "root_execution_id", "execution_id",
    "plan_id", "step_id", "step_index", "iteration_id", "chat_session_id", "chat_turn_id",
    "user_message_id", "scope_resolution", "workload_class", "call_role", "response_reused", "state",
)
ATTEMPT_FIELDS = (
    "schema_version", "fact_schema_version", "record_revision", "lifecycle_phase",
    "principal", "workspace", "trace_id", "llm_call_id", "provider_attempt_id",
    "provider_attempt_index", "dispatch_job_id", "parent_call_id", "parent_relation",
    "retry_group_id", "route_decision_id", "task_id", "root_execution_id", "execution_id",
    "plan_id", "step_id", "step_index", "iteration_id", "chat_session_id", "chat_turn_id",
    "user_message_id",
    "scope_resolution", "workload_class", "call_role",
)
GAP_FIELDS = (
    "principal", "workspace", "llm_call_id", "gap_reason", "missing_record_count",
)
STRING_FIELDS = set(CALL_FIELDS + DISPATCH_FIELDS + ATTEMPT_FIELDS + GAP_FIELDS) - {
    "schema_version", "fact_schema_version", "record_revision",
    "provider_attempt_count", "provider_attempt_index", "response_reused",
    "missing_record_count", "step_index",
}


def parquet_files(root: Path) -> list[Path]:
    if not root.exists():
        return []
    if root.is_symlink() or not root.is_dir():
        raise RuntimeError(f"analytics dataset root is not a real directory: {root}")
    for partition in sorted(root.glob("dt=*")):
        if partition.is_symlink() or not partition.is_dir():
            raise RuntimeError(
                f"analytics partition is not a real directory: {partition}"
            )
    files = []
    for path in sorted(root.glob("dt=*/*.parquet")):
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


def ensure_no_symlink_components(base: Path, target: Path) -> None:
    """Reject symlink/special path components below a trusted runtime root."""
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


def discover_scopes(runtime_root: Path, principal: str | None, workspace: str | None) -> list[tuple[str, str, Path]]:
    scopes_root = runtime_root / "scopes"
    ensure_no_symlink_components(runtime_root, scopes_root)
    if principal and workspace:
        validate_scope_component(principal, "principal")
        validate_scope_component(workspace, "workspace")
        scope_root = scopes_root / principal / workspace
        ensure_no_symlink_components(runtime_root, scope_root)
        return [(principal, workspace, scope_root)]
    if not scopes_root.is_dir():
        return []
    scopes: list[tuple[str, str, Path]] = []
    for principal_dir in sorted(scopes_root.iterdir()):
        if principal_dir.is_symlink():
            raise RuntimeError(f"analytics principal scope is a symlink: {principal_dir}")
        if not principal_dir.is_dir():
            continue
        validate_scope_component(principal_dir.name, "principal")
        for workspace_dir in sorted(principal_dir.iterdir()):
            if workspace_dir.is_symlink():
                raise RuntimeError(
                    f"analytics workspace scope is a symlink: {workspace_dir}"
                )
            if workspace_dir.is_dir():
                validate_scope_component(workspace_dir.name, "workspace")
                scopes.append((principal_dir.name, workspace_dir.name, workspace_dir))
    return scopes


def sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _columns(connection: Any, view: str) -> set[str]:
    return {str(row[0]) for row in connection.execute(f"DESCRIBE {view}").fetchall()}


def _projection(columns: set[str], fields: Iterable[str], principal: str, workspace: str) -> str:
    expressions = []
    for field in fields:
        if field in columns:
            expressions.append(f'"{field}"')
        elif field == "principal":
            expressions.append(f"{sql_string(principal)} AS principal")
        elif field == "workspace":
            expressions.append(f"{sql_string(workspace)} AS workspace")
        elif field in {"schema_version", "fact_schema_version", "record_revision", "provider_attempt_count", "provider_attempt_index"}:
            expressions.append(f"0::INTEGER AS {field}")
        elif field == "step_index":
            expressions.append("NULL::BIGINT AS step_index")
        elif field == "missing_record_count":
            expressions.append("0::BIGINT AS missing_record_count")
        elif field == "response_reused":
            expressions.append("FALSE::BOOLEAN AS response_reused")
        else:
            expressions.append(f"NULL::VARCHAR AS {field}")
    return ", ".join(expressions)


def _read_rows(
    connection: Any,
    root: Path,
    fields: tuple[str, ...],
    principal: str,
    workspace: str,
    view: str,
    *,
    trusted_scope_root: Path | None = None,
) -> tuple[list[dict[str, Any]], set[str]]:
    if trusted_scope_root is not None:
        ensure_no_symlink_components(trusted_scope_root, root)
    files = parquet_files(root)
    if not files:
        return [], set()
    sources = "[" + ",".join(sql_string(str(path)) for path in files) + "]"
    connection.execute(
        f"CREATE OR REPLACE TEMP VIEW {view} AS SELECT * FROM read_parquet("
        f"{sources}, union_by_name=true, hive_partitioning=false)"
    )
    columns = _columns(connection, view)
    projection = _projection(columns, fields, principal, workspace)
    cursor = connection.execute(f"SELECT {projection} FROM {view}")
    rows = [dict(zip(fields, row, strict=True)) for row in cursor.fetchall()]
    for row in rows:
        row["_source_principal"] = principal
        row["_source_workspace"] = workspace
    return rows, columns


def _nonempty(value: Any) -> bool:
    return value is not None and str(value).strip() != ""


def _invalid_identifier(value: Any, *, required: bool) -> bool:
    if value is None:
        return required
    text = str(value)
    return not text or text.strip() != text


def _phase1_call(row: dict[str, Any]) -> bool:
    return (
        int(row.get("schema_version") or 0) >= 1
        or int(row.get("fact_schema_version") or 0) >= 1
        or _nonempty(row.get("llm_call_id"))
    )


def _coalesce_canonical_calls(calls: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Use one latest canonical call revision and suppress its legacy mirror.

    Phase 2 stores start/completion as immutable rows beside the temporary
    legacy compatibility mirror. Treating those storage revisions as separate
    provider calls creates false duplicate failures in the Phase 1 audit.
    """
    canonical: dict[tuple[tuple[str, str], str], dict[str, Any]] = {}
    legacy: list[dict[str, Any]] = []
    for row in calls:
        if int(row.get("record_revision") or 0) > 0 and _nonempty(row.get("llm_call_id")):
            key = (_scope(row), str(row["llm_call_id"]))
            previous = canonical.get(key)
            if previous is None or int(row.get("record_revision") or 0) > int(previous.get("record_revision") or 0):
                canonical[key] = row
        else:
            legacy.append(row)
    canonical_keys = set(canonical)
    return list(canonical.values()) + [
        row for row in legacy
        if not _nonempty(row.get("llm_call_id"))
        or (_scope(row), str(row["llm_call_id"])) not in canonical_keys
    ]


def _coalesce_canonical_attempts(attempts: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Return one latest immutable revision per scoped physical attempt."""
    latest: dict[tuple[tuple[str, str], str], dict[str, Any]] = {}
    passthrough: list[dict[str, Any]] = []
    for row in attempts:
        attempt_id = str(row.get("provider_attempt_id") or "").strip()
        revision = int(row.get("record_revision") or 0)
        if not attempt_id or revision <= 0:
            passthrough.append(row)
            continue
        key = (_scope(row), attempt_id)
        previous = latest.get(key)
        if previous is None or revision > int(previous.get("record_revision") or 0):
            latest[key] = row
    return list(latest.values()) + passthrough


def _scope(row: dict[str, Any]) -> tuple[str, str]:
    return (
        str(row.get("_source_principal", row.get("principal")) or ""),
        str(row.get("_source_workspace", row.get("workspace")) or ""),
    )


def _has_parent_cycle(
    parent_by_call: dict[tuple[tuple[str, str], str], tuple[tuple[str, str], str]],
) -> bool:
    for start in parent_by_call:
        seen: set[tuple[tuple[str, str], str]] = set()
        current = start
        while current in parent_by_call:
            if current in seen:
                return True
            seen.add(current)
            current = parent_by_call[current]
    return False


def audit_rows(
    calls: list[dict[str, Any]],
    dispatch: list[dict[str, Any]],
    attempts: list[dict[str, Any]] | None = None,
    gaps: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    duplicate_canonical_call_revisions = sorted({
        f"{scope[0]}/{scope[1]}:{call_id}:r{revision}"
        for (scope, call_id, revision), count in Counter(
            (_scope(row), str(row.get("llm_call_id") or ""), int(row.get("record_revision") or 0))
            for row in calls
            if int(row.get("record_revision") or 0) > 0
            and _nonempty(row.get("llm_call_id"))
        ).items()
        if count > 1
    })
    duplicate_canonical_attempt_revisions = sorted({
        f"{scope[0]}/{scope[1]}:{attempt_id}:r{revision}"
        for (scope, attempt_id, revision), count in Counter(
            (_scope(row), str(row.get("provider_attempt_id") or ""), int(row.get("record_revision") or 0))
            for row in (attempts or [])
            if int(row.get("record_revision") or 0) > 0
            and _nonempty(row.get("provider_attempt_id"))
        ).items()
        if count > 1
    })
    phase1_calls = [row for row in _coalesce_canonical_calls(calls) if _phase1_call(row)]
    phase1_dispatch = [row for row in dispatch if _nonempty(row.get("llm_call_id"))]
    phase1_attempts = [
        row for row in _coalesce_canonical_attempts(attempts or [])
        if _nonempty(row.get("llm_call_id")) or _nonempty(row.get("provider_attempt_id"))
    ]
    call_ids = {str(row["llm_call_id"]) for row in phase1_calls if _nonempty(row.get("llm_call_id"))}
    dispatch_ids = {str(row["llm_call_id"]) for row in phase1_dispatch if _nonempty(row.get("llm_call_id"))}
    scoped_call_ids = {
        (_scope(row), str(row["llm_call_id"]))
        for row in phase1_calls
        if _nonempty(row.get("llm_call_id"))
    }
    scoped_dispatch_ids = {
        (_scope(row), str(row["llm_call_id"]))
        for row in phase1_dispatch
        if _nonempty(row.get("llm_call_id"))
    }

    missing_call_identity = sum(
        1 for row in phase1_calls
        if not _nonempty(row.get("llm_call_id")) or not _nonempty(row.get("trace_id"))
    )
    producers = Counter(
        str(row["llm_call_id"]) for row in phase1_calls
        if _nonempty(row.get("llm_call_id")) and not bool(row.get("response_reused"))
    )
    duplicate_producer_call_ids = sorted(call_id for call_id, count in producers.items() if count > 1)

    scopes_by_call: dict[str, set[tuple[str, str]]] = defaultdict(set)
    for row in phase1_calls + phase1_dispatch + phase1_attempts:
        if _nonempty(row.get("llm_call_id")):
            scopes_by_call[str(row["llm_call_id"])].add(_scope(row))
    cross_scope_call_ids = sorted(call_id for call_id, scopes in scopes_by_call.items() if len(scopes) > 1)

    dispatch_lookup = Counter(
        (_scope(row), str(row.get("job_id") or ""), str(row.get("llm_call_id") or ""))
        for row in phase1_dispatch
    )
    queued_calls = [row for row in phase1_calls if _nonempty(row.get("dispatch_job_id"))]
    exact_joined_calls = sum(
        1 for row in queued_calls
        if dispatch_lookup[(_scope(row), str(row["dispatch_job_id"]), str(row.get("llm_call_id") or ""))] == 1
    )
    ambiguous_or_orphan_queued_calls = len(queued_calls) - exact_joined_calls
    join_rate = exact_joined_calls / len(queued_calls) if queued_calls else 1.0
    terminal_dispatch = [
        row for row in phase1_dispatch
        if str(row.get("state") or "") in {"completed", "failed", "tombstoned"}
    ]
    terminal_dispatch_without_call = sorted({
        f"{_scope(row)[0]}/{_scope(row)[1]}:{row.get('job_id') or 'unknown'}"
        for row in terminal_dispatch
        if (_scope(row), str(row.get("llm_call_id") or "")) not in scoped_call_ids
    })

    mismatched_queue_correlations: list[str] = []
    dispatch_by_join = {
        (_scope(row), str(row.get("job_id") or ""), str(row.get("llm_call_id") or "")): row
        for row in phase1_dispatch
    }
    for row in queued_calls:
        key = (_scope(row), str(row.get("dispatch_job_id") or ""), str(row.get("llm_call_id") or ""))
        peer = dispatch_by_join.get(key)
        if peer is None:
            continue
        fields = ["trace_id", "provider_attempt_count"]
        # Legacy compatibility rows duplicated the final attempt identity on
        # the call. Canonical call facts intentionally do not; attempt identity
        # is proved against the provider-attempt dataset below.
        if _nonempty(row.get("provider_attempt_id")):
            fields.append("provider_attempt_id")
        for field in fields:
            if row.get(field) != peer.get(field):
                mismatched_queue_correlations.append(f"{row.get('llm_call_id')}:{field}")

    malformed_attempt_ids: list[str] = []
    missing_attempt_ids = 0
    canonical_calls_missing_terminal_attempt: Counter[
        tuple[tuple[str, str], str]
    ] = Counter()
    seen_attempt_ids: Counter[tuple[tuple[str, str], str]] = Counter()
    # Validate legacy duplicated identities and terminal dispatch identities.
    # Canonical call rows are excluded because their provider attempt identity
    # is normalized into `llm_provider_attempts`.
    legacy_and_dispatch = [
        row for row in phase1_calls
        if int(row.get("record_revision") or 0) <= 0
    ] + phase1_dispatch
    for row in legacy_and_dispatch:
        count = int(row.get("provider_attempt_count") or 0)
        # A logical coordinator (for example one chunked aggregate request)
        # can have no provider attempt of its own. Actual direct/queued calls
        # carry count > 0. Pre-provider failures and tombstones legitimately
        # have zero physical attempts even though the logical call terminated.
        attempted = count > 0
        attempt_id = str(row.get("provider_attempt_id") or "")
        call_id = str(row.get("llm_call_id") or "")
        if attempted and not attempt_id:
            missing_attempt_ids += 1
            continue
        if attempt_id:
            # A reused response intentionally carries the producer's exact
            # receipt. It proves attribution but is not another provider
            # attempt producer and must not inflate duplicate-attempt counts.
            if not bool(row.get("response_reused")):
                seen_attempt_ids[(_scope(row), attempt_id)] += 1
            if count <= 0 or attempt_id != f"{call_id}:a{count}":
                malformed_attempt_ids.append(attempt_id)
    canonical_attempt_keys: set[tuple[tuple[str, str], str, str]] = set()
    for row in phase1_attempts:
        call_id = str(row.get("llm_call_id") or "")
        attempt_id = str(row.get("provider_attempt_id") or "")
        index = int(row.get("provider_attempt_index") or 0)
        if not call_id or not attempt_id:
            missing_attempt_ids += 1
            continue
        canonical_attempt_keys.add((_scope(row), call_id, attempt_id))
        if index <= 0 or attempt_id != f"{call_id}:a{index}":
            malformed_attempt_ids.append(attempt_id)
    terminal_dispatch_attempt_keys = {
        (_scope(row), str(row.get("llm_call_id") or ""), str(row.get("provider_attempt_id") or ""))
        for row in terminal_dispatch
        if _nonempty(row.get("provider_attempt_id"))
    }
    for row in phase1_calls:
        if int(row.get("record_revision") or 0) <= 0:
            continue
        count = int(row.get("provider_attempt_count") or 0)
        if count <= 0:
            continue
        expected = f"{row.get('llm_call_id')}:a{count}"
        identity = (_scope(row), str(row.get("llm_call_id") or ""), expected)
        if identity not in canonical_attempt_keys and identity not in terminal_dispatch_attempt_keys:
            canonical_calls_missing_terminal_attempt[
                (_scope(row), str(row.get("llm_call_id") or ""))
            ] += 1
    terminal_gap_counts: Counter[tuple[tuple[str, str], str]] = Counter()
    for row in gaps or []:
        if str(row.get("gap_reason") or "") == "terminal_provider_attempt_lifecycle_unavailable":
            terminal_gap_counts[
                (_scope(row), str(row.get("llm_call_id") or ""))
            ] += int(row.get("missing_record_count") or 0)
    documented_unavailable_attempt_ids = 0
    for owner, missing in canonical_calls_missing_terminal_attempt.items():
        documented = min(missing, terminal_gap_counts[owner])
        documented_unavailable_attempt_ids += documented
        missing_attempt_ids += missing - documented
    duplicate_attempt_ids = sorted(
        attempt_id for (_, attempt_id), count in seen_attempt_ids.items()
        # One legacy call row plus one terminal dispatch row is expected.
        if count > 2
    )

    parent_by_call = {
        (_scope(row), str(row["llm_call_id"])): (_scope(row), str(row["parent_call_id"]))
        for row in phase1_calls + phase1_dispatch + phase1_attempts
        if _nonempty(row.get("llm_call_id")) and _nonempty(row.get("parent_call_id"))
    }
    all_scoped_call_ids = scoped_call_ids | scoped_dispatch_ids
    orphan_parent_ids = sorted({
        f"{scope[0]}/{scope[1]}:{parent}"
        for scope, parent in parent_by_call.values()
        if (scope, parent) not in all_scoped_call_ids
    })
    missing_parent_relations = sorted({
        str(row.get("llm_call_id"))
        for row in phase1_calls + phase1_dispatch + phase1_attempts
        if _nonempty(row.get("parent_call_id")) and not _nonempty(row.get("parent_relation"))
    })
    relation_without_parents = sorted({
        str(row.get("llm_call_id"))
        for row in phase1_calls + phase1_dispatch + phase1_attempts
        if _nonempty(row.get("parent_relation")) and not _nonempty(row.get("parent_call_id"))
    })
    retry_groups_without_member = sorted({
        f"{_scope(row)[0]}/{_scope(row)[1]}:{row['retry_group_id']}"
        for row in phase1_calls
        if _nonempty(row.get("retry_group_id"))
        and (_scope(row), str(row["retry_group_id"])) not in scoped_call_ids
        and not any(
            _scope(other) == _scope(row)
            and str(other.get("retry_group_id") or "") == str(row["retry_group_id"])
            for other in phase1_calls
            if other is not row
        )
    })
    self_referencing_retry_groups = sorted({
        f"{_scope(row)[0]}/{_scope(row)[1]}:{row['llm_call_id']}"
        for row in phase1_calls
        if _nonempty(row.get("llm_call_id"))
        and str(row.get("retry_group_id") or "") == str(row["llm_call_id"])
    })
    reused_without_dispatch = sum(
        1 for row in phase1_calls
        if bool(row.get("response_reused")) and not _nonempty(row.get("dispatch_job_id"))
    )
    invalid_scopes = sorted({
        str(row.get("llm_call_id") or row.get("job_id") or "unknown")
        for row in phase1_calls + phase1_dispatch + phase1_attempts
        if not all(_nonempty(value) for value in _scope(row))
    })
    valid_scope_resolutions = {"explicit", "inherited", "system_default", "legacy_default"}
    invalid_scope_resolutions = sorted({
        str(row.get("llm_call_id") or row.get("job_id") or "unknown")
        for row in phase1_calls + phase1_dispatch + phase1_attempts
        if str(row.get("scope_resolution") or "") not in valid_scope_resolutions
    })
    context_fields = (
        "trace_id",
        "parent_call_id",
        "parent_relation",
        "retry_group_id",
        "route_decision_id",
        "task_id",
        "root_execution_id",
        "execution_id",
        "plan_id",
        "step_id",
        "step_index",
        "iteration_id",
        "chat_session_id",
        "chat_turn_id",
        "user_message_id",
        "scope_resolution",
        "workload_class",
        "call_role",
    )
    context_values: dict[tuple[tuple[str, str], str, str], set[str]] = defaultdict(set)
    for row in phase1_calls + phase1_dispatch + phase1_attempts:
        call_id = str(row.get("llm_call_id") or "")
        if not _nonempty(call_id):
            continue
        for field in context_fields:
            if _nonempty(row.get(field)):
                context_values[(_scope(row), call_id, field)].add(str(row[field]))
    context_drift_fields = sorted(
        field
        for (_, _, field), values in context_values.items()
        if len(values) > 1
    )
    traces_by_call: dict[tuple[tuple[str, str], str], set[str]] = defaultdict(set)
    for (scope, call_id, field), values in context_values.items():
        if field == "trace_id":
            traces_by_call[(scope, call_id)].update(values)
    parent_identity_mismatches = sorted({
        child
        for (scope, child), (_, parent) in parent_by_call.items()
        if (scope, parent) in traces_by_call
        and traces_by_call[(scope, child)] != traces_by_call[(scope, parent)]
    })
    embedded_scope_mismatches = sum(
        1
        for row in phase1_calls + phase1_dispatch + phase1_attempts
        if "_source_principal" in row
        and (
            str(row.get("principal") or "") != str(row["_source_principal"])
            or str(row.get("workspace") or "") != str(row["_source_workspace"])
        )
    )
    noncanonical_identity_fields: list[str] = []
    identity_sets = (
        (
            "calls",
            phase1_calls,
            ("trace_id", "llm_call_id"),
            (
                "provider_attempt_id",
                "dispatch_job_id",
                "parent_call_id",
                "retry_group_id",
                "route_decision_id",
                "task_id",
                "root_execution_id",
                "execution_id",
                "plan_id",
                "step_id",
                "iteration_id",
                "chat_session_id",
                "chat_turn_id",
                "user_message_id",
            ),
        ),
        (
            "dispatch",
            phase1_dispatch,
            ("trace_id", "llm_call_id", "job_id"),
            (
                "provider_attempt_id",
                "parent_call_id",
                "retry_group_id",
                "route_decision_id",
                "task_id",
                "root_execution_id",
                "execution_id",
                "plan_id",
                "step_id",
                "iteration_id",
                "chat_session_id",
                "chat_turn_id",
                "user_message_id",
            ),
        ),
        (
            "attempts",
            phase1_attempts,
            ("trace_id", "llm_call_id", "provider_attempt_id"),
            (
                "dispatch_job_id",
                "parent_call_id",
                "retry_group_id",
                "route_decision_id",
                "task_id",
                "root_execution_id",
                "execution_id",
                "plan_id",
                "step_id",
                "iteration_id",
                "chat_session_id",
                "chat_turn_id",
                "user_message_id",
            ),
        ),
        ("gaps", gaps or [], (), ("llm_call_id",)),
    )
    for dataset, rows, required_fields, optional_fields in identity_sets:
        for row in rows:
            for field in required_fields:
                if _invalid_identifier(row.get(field), required=True):
                    noncanonical_identity_fields.append(f"{dataset}:{field}")
            for field in optional_fields:
                if row.get(field) is not None and _invalid_identifier(
                    row.get(field), required=False
                ):
                    noncanonical_identity_fields.append(f"{dataset}:{field}")

    failures = {
        "missing_call_identity": missing_call_identity,
        "duplicate_producer_call_ids": duplicate_producer_call_ids,
        "duplicate_canonical_call_revisions": duplicate_canonical_call_revisions,
        "duplicate_canonical_attempt_revisions": duplicate_canonical_attempt_revisions,
        "cross_scope_call_ids": cross_scope_call_ids,
        "ambiguous_or_orphan_queued_calls": ambiguous_or_orphan_queued_calls,
        "terminal_dispatch_without_call": terminal_dispatch_without_call,
        "mismatched_queue_correlations": sorted(set(mismatched_queue_correlations)),
        "missing_attempt_ids": missing_attempt_ids,
        "malformed_attempt_ids": sorted(set(malformed_attempt_ids)),
        "duplicate_attempt_ids": duplicate_attempt_ids,
        "orphan_parent_ids": orphan_parent_ids,
        "missing_parent_relations": missing_parent_relations,
        "relation_without_parents": relation_without_parents,
        "parent_cycle": _has_parent_cycle(parent_by_call),
        "retry_groups_without_member": retry_groups_without_member,
        "self_referencing_retry_groups": self_referencing_retry_groups,
        "reused_without_dispatch": reused_without_dispatch,
        "invalid_scopes": invalid_scopes,
        "invalid_scope_resolutions": invalid_scope_resolutions,
        "context_drift_fields": context_drift_fields,
        "parent_identity_mismatches": parent_identity_mismatches,
        "embedded_scope_mismatches": embedded_scope_mismatches,
        "noncanonical_identity_fields": sorted(noncanonical_identity_fields),
    }
    has_failures = any(bool(value) for value in failures.values()) or join_rate < 0.99
    status = "no_phase1_data" if not phase1_calls and not phase1_dispatch and not phase1_attempts else ("failed" if has_failures else "passed")
    return {
        "status": status,
        "phase1_call_rows": len(phase1_calls),
        "phase1_dispatch_rows": len(phase1_dispatch),
        "phase1_attempt_rows": len(phase1_attempts),
        "documented_unavailable_attempt_ids": documented_unavailable_attempt_ids,
        "unique_call_ids": len(call_ids | dispatch_ids),
        "queued_call_rows": len(queued_calls),
        "direct_call_rows": len(phase1_calls) - len(queued_calls),
        "exact_joined_call_rows": exact_joined_calls,
        "exact_join_rate": round(join_rate, 6),
        "reused_call_rows": sum(bool(row.get("response_reused")) for row in phase1_calls),
        "failures": failures,
    }


def render_html(report: dict[str, Any]) -> str:
    audit = report["audit"]
    failures = audit["failures"]
    failure_rows = "".join(
        f"<tr><td><code>{html.escape(name)}</code></td><td>{html.escape(json.dumps(value))}</td></tr>"
        for name, value in failures.items()
    )
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>LLM observability Phase 1 correlation audit</title><style>
body{{font-family:Inter,system-ui,sans-serif;background:#0b1020;color:#e8ecf7;margin:0;padding:32px}}main{{max-width:1100px;margin:auto}}
.card{{background:#121a2e;border:1px solid #273452;border-radius:14px;padding:18px;margin:16px 0}}table{{width:100%;border-collapse:collapse}}
th,td{{text-align:left;border-bottom:1px solid #273452;padding:9px}}th{{color:#9fb3d9}}code{{color:#9dd9ff}}.status{{display:inline-block;padding:5px 10px;border-radius:999px;background:#183d32;color:#8af0c4}}
</style></head><body><main><h1>LLM observability Phase 1 correlation audit</h1>
<section class="card"><span class="status">{html.escape(audit['status'])}</span><p>Generated {html.escape(report['generated_at'])}. Content-free identity and join audit.</p></section>
<section class="card"><h2>Coverage</h2><p>{audit['phase1_call_rows']:,} call rows · {audit['phase1_attempt_rows']:,} physical-attempt rows · {audit['phase1_dispatch_rows']:,} dispatch rows · {audit['unique_call_ids']:,} unique calls</p>
<p>Queued exact join: {audit['exact_joined_call_rows']:,}/{audit['queued_call_rows']:,} ({audit['exact_join_rate'] * 100:.2f}%). Direct rows: {audit['direct_call_rows']:,}. Reused rows: {audit['reused_call_rows']:,}. Attempt identities explicitly unavailable: {audit['documented_unavailable_attempt_ids']:,}.</p></section>
<section class="card"><h2>Invariant failures</h2><table><thead><tr><th>Invariant</th><th>Finding</th></tr></thead><tbody>{failure_rows}</tbody></table></section>
<section class="card"><h2>Privacy</h2><p>No prompt, response, tool argument, attachment, transcript, or artifact content was read or written.</p></section>
</main></body></html>"""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime-root", type=Path, default=Path(os.environ.get("MAGICIAN_ROOT_DIR", "~/MagicianNotes")).expanduser())
    parser.add_argument("--principal")
    parser.add_argument("--workspace")
    parser.add_argument("--output-dir", type=Path, default=Path("coverage/evals/llm-observability-phase1/latest"))
    parser.add_argument("--strict", action="store_true")
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
        print("duckdb Python package is required for the Phase 1 audit", file=sys.stderr)
        return 2

    connection = duckdb.connect()
    calls: list[dict[str, Any]] = []
    dispatch: list[dict[str, Any]] = []
    attempts: list[dict[str, Any]] = []
    gaps: list[dict[str, Any]] = []
    errors: list[str] = []
    for index, (principal, workspace, scope_root) in enumerate(discover_scopes(args.runtime_root, args.principal, args.workspace)):
        try:
            scope_calls, _ = _read_rows(connection, scope_root / "analytics/llm_calls", CALL_FIELDS, principal, workspace, f"phase1_calls_{index}", trusted_scope_root=scope_root)
            calls.extend(scope_calls)
        except Exception as error:
            errors.append(f"{principal}/{workspace} llm_calls: {error}")
        try:
            scope_dispatch, _ = _read_rows(connection, scope_root / "analytics/llm_dispatch", DISPATCH_FIELDS, principal, workspace, f"phase1_dispatch_{index}", trusted_scope_root=scope_root)
            dispatch.extend(scope_dispatch)
        except Exception as error:
            errors.append(f"{principal}/{workspace} llm_dispatch: {error}")
        try:
            scope_attempts, _ = _read_rows(connection, scope_root / "analytics/llm_provider_attempts", ATTEMPT_FIELDS, principal, workspace, f"phase1_attempts_{index}", trusted_scope_root=scope_root)
            attempts.extend(scope_attempts)
        except Exception as error:
            errors.append(f"{principal}/{workspace} llm_provider_attempts: {error}")
        try:
            scope_gaps, _ = _read_rows(connection, scope_root / "analytics/llm_capture_gaps", GAP_FIELDS, principal, workspace, f"phase1_gaps_{index}", trusted_scope_root=scope_root)
            gaps.extend(scope_gaps)
        except Exception as error:
            errors.append(f"{principal}/{workspace} llm_capture_gaps: {error}")
    audit = audit_rows(calls, dispatch, attempts, gaps)
    if errors and audit["status"] == "passed":
        audit["status"] = "partial"
    report = {
        "schema_version": 1,
        "evaluation": "llm-observability-phase1-correlation",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "runtime_root": str(args.runtime_root),
        "audit": audit,
        "errors": errors,
        "privacy": {"content_fields_read": [], "raw_payloads_written": False},
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.output_dir / "report.json"
    html_path = args.output_dir / "report.html"
    json_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    html_path.write_text(render_html(report), encoding="utf-8")
    print(f"LLM observability Phase 1: {audit['status']}; calls={audit['phase1_call_rows']}; join={audit['exact_join_rate'] * 100:.2f}%")
    print(f"Report: {html_path.resolve().as_uri()}")
    return 1 if args.strict and audit["status"] != "passed" else 0


if __name__ == "__main__":
    raise SystemExit(main())

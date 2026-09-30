#!/usr/bin/env python3
"""Live privacy/restricted-access audit for Phase 3 sanitized LLM capture.

The evaluator reads restricted Parquet locally but emits only counts, bounded
machine categories, and pass/fail evidence. It never copies model content into
JSON, HTML, stdout, or a failure message. When both ``--call-id`` and the setup
token environment variable are present it also exercises the one-use grant API.
"""

from __future__ import annotations

import argparse
import html
import json
import os
from pathlib import Path
import re
import sys
import time
from typing import Any, Iterable
import urllib.error
import urllib.request
from datetime import datetime, timezone


PHASES = {"logical_request", "effective_request", "normalized_response"}
FORBIDDEN_KEYS = {
    "raw_response",
    "raw_provider_body",
    "response_id",
    "reasoning_text",
    "chain_of_thought",
}
SENSITIVE_KEYS = {
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "cookie",
    "private_key",
    "credential",
}
SECRET_PATTERNS = {
    "private_key": re.compile(
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----", re.IGNORECASE
    ),
    "bearer_token": re.compile(r"\bbearer\s+[A-Za-z0-9._~+/=-]{8,}", re.IGNORECASE),
    "credential_value": re.compile(
        r"\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password|passwd|authorization|cookie)\b\s*[:=]\s*[\"']?[^\s,;\"']{4,}",
        re.IGNORECASE,
    ),
    "common_token": re.compile(
        r"\b(?:sk|pk|ghp|gho|github_pat|xoxb|xoxp|xoxa|xoxr)-[A-Za-z0-9_-]{12,}\b"
    ),
    "binary_data_url": re.compile(r"data:[^;,]{1,128};base64,", re.IGNORECASE),
}
ADVERSARIAL_CANARIES = (
    "must-never-return",
    "fixture-secret",
    "known-secret-value",
    "do-not-store",
)
MAX_FILES = 10_000
MAX_AUDIT_ROWS = 10_000


def validate_scope_component(value: str, label: str) -> None:
    if (
        not value
        or value.strip() != value
        or len(value.encode("utf-8")) > 255
        or value in {".", ".."}
        or any(
            char in {'/', '\\', ':', '*', '?', '"', '<', '>', '|'}
            or ord(char) < 32
            for char in value
        )
    ):
        raise RuntimeError(f"invalid {label} scope component")


def ensure_real_path(root: Path, target: Path) -> None:
    try:
        relative = target.relative_to(root)
    except ValueError as error:
        raise RuntimeError("restricted dataset path escapes runtime root") from error
    current = root
    for component in relative.parts:
        current = current / component
        if not current.exists() and not current.is_symlink():
            return
        if current.is_symlink() or not current.is_dir():
            raise RuntimeError("restricted dataset path contains a symlink or special object")


def parquet_files(runtime_root: Path, dataset_root: Path) -> list[Path]:
    ensure_real_path(runtime_root, dataset_root)
    if not dataset_root.exists():
        return []
    files: list[Path] = []
    for partition in sorted(dataset_root.iterdir()):
        if not partition.name.startswith("dt="):
            continue
        if partition.is_symlink() or not partition.is_dir():
            raise RuntimeError("restricted UTC partition is not a real directory")
        for candidate in sorted(partition.iterdir()):
            if candidate.suffix != ".parquet":
                continue
            if candidate.is_symlink() or not candidate.is_file():
                raise RuntimeError("restricted Parquet source is not a regular file")
            files.append(candidate)
            if len(files) > MAX_FILES:
                raise RuntimeError("restricted Parquet file budget exceeded")
    return files


def sql_list(files: Iterable[Path]) -> str:
    return "[" + ",".join("'" + str(path).replace("'", "''") + "'" for path in files) + "]"


def read_call_io_rows(
    runtime_root: Path,
    principal: str,
    workspace: str,
    from_ms: int,
    to_ms: int,
) -> list[dict[str, Any]]:
    try:
        import duckdb  # type: ignore
    except ImportError as error:
        raise RuntimeError("Python duckdb is required for the live Phase 3 audit") from error
    root = runtime_root / "scopes" / principal / workspace / "analytics" / "llm_call_io"
    files = parquet_files(runtime_root, root)
    if not files:
        return []
    connection = duckdb.connect(":memory:")
    try:
        cursor = connection.execute(
            "SELECT principal, workspace, llm_call_id, content_phase, "
            "provider_attempt_index, capture_status, redaction_version, "
            "redaction_count, content_fingerprint, original_bytes, "
            "sanitized_bytes, restricted_payload_json, observed_at_ms "
            f"FROM read_parquet({sql_list(files)}, union_by_name=true, hive_partitioning=false) "
            "WHERE observed_at_ms BETWEEN ? AND ? ORDER BY observed_at_ms "
            f"LIMIT {MAX_AUDIT_ROWS + 1}",
            [from_ms, to_ms],
        )
        names = [item[0] for item in cursor.description]
        rows = [dict(zip(names, row, strict=True)) for row in cursor.fetchall()]
        if len(rows) > MAX_AUDIT_ROWS:
            raise RuntimeError(
                "restricted storage audit row budget exceeded; narrow --window-hours"
            )
        return rows
    finally:
        connection.close()


def normalized_key(key: str) -> str:
    return key.lower().replace("-", "_").replace(" ", "_")


def sensitive_key(key: str) -> bool:
    value = normalized_key(key)
    return any(value == item or value.endswith("_" + item) for item in SENSITIVE_KEYS)


def scan_payload(value: Any) -> dict[str, int]:
    violations: dict[str, int] = {}

    def note(category: str) -> None:
        violations[category] = violations.get(category, 0) + 1

    def visit(item: Any, parent_key: str | None = None) -> None:
        if isinstance(item, dict):
            for key, child in item.items():
                key_text = str(key)
                lower = normalized_key(key_text)
                if lower in FORBIDDEN_KEYS:
                    note("forbidden_raw_field")
                if sensitive_key(key_text) and child is not None and child not in (
                    "[REDACTED]",
                    "[REDACTED_KNOWN_SECRET]",
                ):
                    note("sensitive_field_not_redacted")
                key_lowered = key_text.lower()
                if any(canary in key_lowered for canary in ADVERSARIAL_CANARIES):
                    note("adversarial_canary_in_key")
                if SECRET_PATTERNS["common_token"].search(key_text):
                    note("common_token_in_key")
                if lower == "raw_provider_body_captured" and child is not False:
                    note("raw_provider_body_claimed")
                if lower == "content_captured" and parent_key == "reasoning" and child is not False:
                    note("reasoning_content_claimed")
                visit(child, lower)
        elif isinstance(item, list):
            for child in item:
                visit(child, parent_key)
        elif isinstance(item, str):
            scan_text = item.replace("[REDACTED]", "").replace(
                "[REDACTED_KNOWN_SECRET]", ""
            )
            for category, pattern in SECRET_PATTERNS.items():
                if pattern.search(scan_text):
                    note(category)
            lowered = item.lower()
            if any(canary in lowered for canary in ADVERSARIAL_CANARIES):
                note("adversarial_canary")

    visit(value)
    return violations


def audit_rows(
    rows: list[dict[str, Any]], principal: str, workspace: str
) -> dict[str, Any]:
    violations: dict[str, int] = {}
    phases: dict[str, int] = {phase: 0 for phase in sorted(PHASES)}
    calls: set[str] = set()
    redacted = 0
    oversize = 0

    def note(category: str, count: int = 1) -> None:
        violations[category] = violations.get(category, 0) + count

    for row in rows:
        if row.get("principal") != principal or row.get("workspace") != workspace:
            note("scope_mismatch")
        call_id = str(row.get("llm_call_id") or "")
        if not call_id:
            note("missing_call_id")
        else:
            calls.add(call_id)
        phase = str(row.get("content_phase") or "")
        if phase not in PHASES:
            note("unknown_phase")
        else:
            phases[phase] += 1
        if phase == "logical_request" and row.get("provider_attempt_index") is not None:
            note("logical_request_has_attempt")
        if phase != "logical_request" and not int(row.get("provider_attempt_index") or 0):
            note("attempt_phase_missing_attempt")
        fingerprint = str(row.get("content_fingerprint") or "")
        if not re.fullmatch(r"[0-9a-f]{64}", fingerprint):
            note("invalid_fingerprint")
        if not str(row.get("redaction_version") or ""):
            note("missing_redaction_version")
        capture_status = str(row.get("capture_status") or "")
        if capture_status == "redacted":
            redacted += 1
        elif capture_status == "oversize":
            oversize += 1
        elif capture_status != "complete":
            note("invalid_capture_status")
        payload_json = str(row.get("restricted_payload_json") or "")
        if len(payload_json.encode("utf-8")) > 4 * 1024 * 1024:
            note("payload_hard_limit_exceeded")
            continue
        try:
            payload = json.loads(payload_json)
        except json.JSONDecodeError:
            note("payload_json_invalid")
            continue
        for category, count in scan_payload(payload).items():
            note(category, count)

    return {
        "row_count": len(rows),
        "call_count": len(calls),
        "phase_counts": phases,
        "redacted_rows": redacted,
        "oversize_rows": oversize,
        "violation_counts": dict(sorted(violations.items())),
        "violation_count": sum(violations.values()),
    }


def request_json(url: str, method: str, headers: dict[str, str], body: dict[str, Any] | None) -> tuple[dict[str, Any], Any]:
    payload = None if body is None else json.dumps(body).encode("utf-8")
    request = urllib.request.Request(url, data=payload, method=method, headers=headers)
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.loads(response.read().decode("utf-8")), response.headers


def probe_restricted_api(
    api_base: str,
    principal: str,
    workspace: str,
    call_id: str,
    setup_token: str,
) -> dict[str, Any]:
    headers = {
        "Content-Type": "application/json",
        "X-Magician-Setup-Token": setup_token,
    }
    bearer = os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
    if bearer:
        headers["Authorization"] = f"Bearer {bearer}"
    grant, grant_headers = request_json(
        f"{api_base.rstrip('/')}/api/magician/v2/analytics/llm/content/grants",
        "POST",
        headers,
        {"llm_call_id": call_id, "reason": "phase3_live_privacy_audit", "ttl_secs": 60},
    )
    token = str(grant.get("token") or "")
    if len(token) < 32:
        raise RuntimeError("restricted grant response omitted a bounded token")
    read_headers = {
        "X-Magician-Content-Grant": token,
    }
    if bearer:
        read_headers["Authorization"] = f"Bearer {bearer}"
    content, content_headers = request_json(
        f"{api_base.rstrip('/')}/api/magician/v2/analytics/llm/content/calls/{call_id}",
        "GET",
        read_headers,
        None,
    )
    violations = scan_payload(content)
    cache_control = str(content_headers.get("Cache-Control") or "").lower()
    grant_cache_control = str(grant_headers.get("Cache-Control") or "").lower()
    if "no-store" not in cache_control or "no-store" not in grant_cache_control:
        violations["missing_no_store"] = violations.get("missing_no_store", 0) + 1
    # The exact same token must be unusable after one successful reveal.
    reuse_denied = False
    try:
        request_json(
            f"{api_base.rstrip('/')}/api/magician/v2/analytics/llm/content/calls/{call_id}",
            "GET",
            read_headers,
            None,
        )
    except urllib.error.HTTPError as error:
        reuse_denied = error.code == 401
    if not reuse_denied:
        violations["grant_reuse_not_denied"] = violations.get("grant_reuse_not_denied", 0) + 1
    return {
        "performed": True,
        "revision_count": len(content.get("revisions") or []),
        "one_use_reuse_denied": reuse_denied,
        "violation_counts": dict(sorted(violations.items())),
        "violation_count": sum(violations.values()),
    }


def render_html(report: dict[str, Any]) -> str:
    audit = report["storage_audit"]
    status = report["status"]
    status_class = "ok" if status == "passed" else "skip" if status == "skipped" else "bad"
    violation_rows = "".join(
        f"<tr><td>{html.escape(name)}</td><td>{count}</td></tr>"
        for name, count in audit.get("violation_counts", {}).items()
    ) or '<tr><td colspan="2">None</td></tr>'
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>LLM observability Phase 3 audit</title><style>
body{{font-family:Inter,system-ui,sans-serif;background:#0b1020;color:#e8ecf7;margin:0;padding:32px}}main{{max-width:1000px;margin:auto}}.card{{background:#121a2e;border:1px solid #273452;border-radius:14px;padding:18px;margin:16px 0}}.status{{display:inline-block;padding:6px 11px;border-radius:999px}}.ok{{background:#183d32;color:#8af0c4}}.bad{{background:#4a202a;color:#ffb3c1}}.skip{{background:#3d3520;color:#ffe099}}.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px}}.metric{{background:#0e1628;border-radius:10px;padding:14px}}.metric b{{display:block;font-size:1.45rem}}table{{width:100%;border-collapse:collapse}}th,td{{text-align:left;padding:8px;border-bottom:1px solid #273452}}
</style></head><body><main><h1>LLM observability Phase 3</h1><section class="card"><span class="status {status_class}">{html.escape(status)}</span><p>Generated {html.escape(report['generated_at'])}. Sanitized storage, privacy, and optional one-use grant audit.</p></section><section class="card grid"><div class="metric"><b>{audit.get('row_count', 0):,}</b>sanitized revisions</div><div class="metric"><b>{audit.get('call_count', 0):,}</b>logical calls</div><div class="metric"><b>{audit.get('redacted_rows', 0):,}</b>redacted rows</div><div class="metric"><b>{audit.get('oversize_rows', 0):,}</b>oversize omissions</div><div class="metric"><b>{audit.get('violation_count', 0):,}</b>privacy violations</div></section><section class="card"><h2>Phases</h2><p>{html.escape(json.dumps(audit.get('phase_counts', {}), sort_keys=True))}</p></section><section class="card"><h2>Violations</h2><table><thead><tr><th>Machine category</th><th>Count</th></tr></thead><tbody>{violation_rows}</tbody></table></section><section class="card"><h2>Restricted API</h2><p>{html.escape(json.dumps(report['restricted_api'], sort_keys=True))}</p></section><section class="card"><h2>Privacy of this report</h2><p>No prompt, response, reasoning, tool argument/result, attachment, URL, token, grant, or payload excerpt is written to this report.</p></section></main></body></html>"""


def self_test() -> None:
    safe = {
        "messages": [{"content": "api_key=[REDACTED]"}],
        "reasoning": {"present": True, "content_captured": False, "fingerprint": "a" * 64},
        "raw_provider_body_captured": False,
        "response_id_fingerprint": "b" * 64,
    }
    assert scan_payload(safe) == {}
    unsafe = {
        "api_key": "live-secret",
        "raw_provider_body_captured": True,
        "reasoning": {"content_captured": True},
        "raw_response": "Bearer abcdefghijklmnop",
    }
    violations = scan_payload(unsafe)
    for expected in (
        "sensitive_field_not_redacted",
        "raw_provider_body_claimed",
        "reasoning_content_claimed",
        "forbidden_raw_field",
        "bearer_token",
    ):
        assert violations.get(expected, 0) > 0


def write_report(output_dir: Path, report: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--runtime-root",
        type=Path,
        default=Path(os.environ.get("MAGICIAN_ROOT_DIR", "~/MagicianNotes")).expanduser(),
    )
    parser.add_argument("--principal", default=os.environ.get("LLM_PHASE3_PRINCIPAL", "anonymous"))
    parser.add_argument("--workspace", default=os.environ.get("LLM_PHASE3_WORKSPACE", "default"))
    parser.add_argument("--api-base", default=os.environ.get("MAGICIAN_URL", "http://127.0.0.1:3002"))
    parser.add_argument("--window-hours", type=float, default=24.0 * 7)
    parser.add_argument("--call-id", default=os.environ.get("LLM_PHASE3_CALL_ID", ""))
    parser.add_argument("--setup-token-env", default="MAGICIAN_SETUP_TOKEN")
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("coverage/evals/llm-observability-phase3/latest"),
    )
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("Phase 3 evaluator self-test passed")
        return 0
    validate_scope_component(args.principal, "principal")
    validate_scope_component(args.workspace, "workspace")
    if args.window_hours <= 0 or args.window_hours > 31 * 24:
        parser.error("--window-hours must be in (0, 744]")
    if args.dry_run:
        print(json.dumps({
            "runtime_root": str(args.runtime_root),
            "scope": f"{args.principal}/{args.workspace}",
            "reads_restricted_content_locally": True,
            "writes_content_to_report": False,
            "restricted_api_probe": bool(args.call_id and os.environ.get(args.setup_token_env)),
        }, indent=2))
        return 0

    to_ms = int(time.time() * 1000)
    from_ms = to_ms - int(args.window_hours * 60 * 60 * 1000)
    try:
        rows = read_call_io_rows(
            args.runtime_root, args.principal, args.workspace, from_ms, to_ms
        )
    except Exception as error:
        print(f"Phase 3 restricted storage audit failed: {error}", file=sys.stderr)
        return 2
    storage_audit = audit_rows(rows, args.principal, args.workspace)
    restricted_api: dict[str, Any] = {"performed": False, "reason": "call_id_or_setup_token_absent"}
    setup_token = os.environ.get(args.setup_token_env, "")
    if args.call_id and setup_token:
        try:
            restricted_api = probe_restricted_api(
                args.api_base,
                args.principal,
                args.workspace,
                args.call_id,
                setup_token,
            )
        except Exception:
            # Never persist HTTP bodies or token-bearing exception details.
            restricted_api = {
                "performed": True,
                "violation_count": 1,
                "violation_counts": {"restricted_api_probe_failed": 1},
            }

    if not rows:
        status = "skipped"
    elif storage_audit["violation_count"] or restricted_api.get("violation_count", 0):
        status = "failed"
    else:
        status = "passed"
    report = {
        "schema_version": 1,
        "evaluation": "llm-observability-phase3-sanitized-content",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "status": status,
        "scope": {"principal": args.principal, "workspace": args.workspace},
        "range": {"from_ms": from_ms, "to_ms": to_ms},
        "storage_audit": storage_audit,
        "restricted_api": restricted_api,
        "privacy": {
            "restricted_payloads_read_locally": True,
            "content_written_to_report": False,
            "content_printed_to_terminal": False,
        },
    }
    write_report(args.output_dir, report)
    print(
        f"LLM observability Phase 3: {status}; rows={storage_audit['row_count']}; "
        f"calls={storage_audit['call_count']}; violations={storage_audit['violation_count']}"
    )
    print(f"Report: {(args.output_dir / 'report.html').resolve().as_uri()}")
    if status == "skipped":
        return 3
    if status == "failed" and args.strict:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

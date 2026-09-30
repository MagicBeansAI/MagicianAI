#!/usr/bin/env python3
"""Export one redacted browser-backed task into the Task Recipes eval corpus.

The live trace store is already header-redacted. This exporter applies a
second fail-closed scrub to headers, cookies, query credentials, bodies, and
operator-provided values before anything enters git.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


SENSITIVE_KEYS = {
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "apikey",
    "access_token",
    "refresh_token",
    "password",
    "passwd",
    "secret",
    "client_secret",
}
QUERY_SECRET_RE = re.compile(
    r"([?&](?:key|api[_-]?key|token|access_token|sig|signature)=)[^&#\s]+",
    re.IGNORECASE,
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--task-id", required=True)
    parser.add_argument(
        "--execution-id",
        action="append",
        default=[],
        help="explicit execution id to export (repeatable); task executions are discovered by default",
    )
    parser.add_argument("--name", required=True)
    parser.add_argument("--scope", default="anonymous/default")
    parser.add_argument("--base", default="http://127.0.0.1:3002")
    parser.add_argument("--runtime-root", type=Path)
    parser.add_argument("--output-root", type=Path, default=Path("magician/tests/fixtures/task_recipes"))
    parser.add_argument("--scrub", action="append", default=[])
    return parser.parse_args()


def validate_id(value: str, label: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", value):
        raise SystemExit(f"unsafe {label}: {value!r}")
    return value


def replace_scrubs(value: str, scrubs: list[str]) -> str:
    result = QUERY_SECRET_RE.sub(r"\1[REDACTED]", value)
    for secret in sorted((item for item in scrubs if item), key=len, reverse=True):
        result = result.replace(secret, "[REDACTED]")
    return result


def scrub(value: Any, scrubs: list[str], key: str = "") -> Any:
    if key.lower() in SENSITIVE_KEYS:
        return "[REDACTED]"
    if isinstance(value, str):
        return replace_scrubs(value, scrubs)
    if isinstance(value, list):
        return [scrub(item, scrubs) for item in value]
    if isinstance(value, dict):
        return {name: scrub(item, scrubs, str(name)) for name, item in value.items()}
    return value


def request_json(
    base: str, path: str, principal: str, workspace: str
) -> Any:
    request = urllib.request.Request(base.rstrip("/") + path)
    request.add_header("X-Principal", principal)
    request.add_header("X-Workspace", workspace)
    bearer = os.environ.get("MAGICIAN_BEARER") or os.environ.get("MAGICIAN_BEARER_TOKEN")
    if bearer:
        request.add_header("Authorization", f"Bearer {bearer}")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise SystemExit(f"GET {path} failed with HTTP {error.code}") from error
    except OSError as error:
        raise SystemExit(f"GET {path} failed: {error}") from error


def collect_strings(value: Any, key: str) -> set[str]:
    found: set[str] = set()
    if isinstance(value, dict):
        candidate = value.get(key)
        if isinstance(candidate, str) and candidate:
            found.add(candidate)
        for child in value.values():
            found.update(collect_strings(child, key))
    elif isinstance(value, list):
        for child in value:
            found.update(collect_strings(child, key))
    return found


def read_jsonl(paths: list[Path], scrubs: list[str]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for path in sorted(paths):
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if not line.strip():
                continue
            try:
                records.append(scrub(json.loads(line), scrubs))
            except json.JSONDecodeError as error:
                raise SystemExit(f"{path}:{number}: invalid JSON: {error}") from error
    return records


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    task_id = validate_id(args.task_id, "task id")
    name = validate_id(args.name, "fixture name")
    try:
        principal, workspace = args.scope.split("/", 1)
    except ValueError as error:
        raise SystemExit("--scope must be principal/workspace") from error
    validate_id(principal, "principal")
    validate_id(workspace, "workspace")

    runtime_root = args.runtime_root or Path(os.environ.get("MAGICIAN_ROOT_DIR", "~/MagicianNotes")).expanduser()
    # The live runtime root is flat: <root>/scopes, not the legacy
    # <root>/magician_data_v3/scopes seed/package layout.
    mining_root = runtime_root / "scopes" / principal / workspace / "api_mining"
    details = request_json(
        args.base,
        f"/api/magician/v3/tasks/{task_id}/details",
        principal,
        workspace,
    )
    executions = request_json(
        args.base,
        f"/api/magician/v3/tasks/{task_id}/executions",
        principal,
        workspace,
    )
    record_ids = {task_id}
    record_ids.update(
        validate_id(value, "discovered execution id")
        for value in collect_strings(executions, "execution_id")
    )
    record_ids.update(validate_id(value, "execution id") for value in args.execution_id)
    task_roots = [
        root
        for record_id in sorted(record_ids)
        for root in (mining_root / record_id, mining_root / "traces" / record_id)
    ]
    trace_paths = sorted(
        {path for root in task_roots for path in root.glob("trace_*.jsonl")}
    )
    action_paths = sorted(
        {path for root in task_roots for path in root.glob("actions_*.jsonl")}
    )
    if not trace_paths:
        searched = ", ".join(sorted(record_ids))
        raise SystemExit(
            f"no trace_*.jsonl files found for task/execution ids [{searched}] under {mining_root}"
        )

    output_dir = args.output_root / name
    if output_dir.exists() and any(output_dir.iterdir()):
        raise SystemExit(f"refusing to overwrite non-empty fixture directory: {output_dir}")
    output_dir.mkdir(parents=True, exist_ok=True)

    traces = read_jsonl(trace_paths, args.scrub)
    actions = read_jsonl(action_paths, args.scrub)
    (output_dir / "traces.jsonl").write_text(
        "".join(json.dumps(record, sort_keys=True) + "\n" for record in traces), encoding="utf-8"
    )
    (output_dir / "actions.jsonl").write_text(
        "".join(json.dumps(record, sort_keys=True) + "\n" for record in actions), encoding="utf-8"
    )

    task = details.get("task") if isinstance(details.get("task"), dict) else details
    write_json(
        output_dir / "task.json",
        {
            "task_id": task_id,
            "title": scrub(task.get("title", ""), args.scrub),
            "description": scrub(task.get("description", ""), args.scrub),
            "agent_id": task.get("agent_id") or task.get("agentId") or "personal-assistant",
            "principal": principal,
            "workspace": workspace,
            "typed_inputs": sorted(
                {
                    str(value)
                    for event in actions
                    for value in (
                        event.get("user_values")
                        if isinstance(event.get("user_values"), list)
                        else []
                    )
                    if isinstance(value, (str, int, float))
                }
            ),
        },
    )
    summary = details.get("summary") or details.get("result", {}).get("summary") or ""
    artifacts = details.get("artifact_previews") or details.get("artifacts") or []
    outputs = details.get("output_previews") or []
    write_json(
        output_dir / "reported.json",
        scrub({"summary": summary, "artifact_previews": artifacts, "output_previews": outputs}, args.scrub),
    )
    write_json(
        output_dir / "expected.json",
        {
            "compile": "ok",
            "steps": None,
            "inputs": [],
            "data_flows": None,
            "write_steps": None,
            "verify_with_steps": None,
            "answer_fields": [],
            "template": None,
            "replay": None,
            "variants": [],
            "title_variants": [],
            "must_not_contain": sorted(set(args.scrub)),
            "known_failing": "fill expected.json, then remove this marker",
        },
    )
    print(f"wrote redacted fixture skeleton: {output_dir}")
    print("review every file manually before adding it to git")
    return 0


if __name__ == "__main__":
    sys.exit(main())

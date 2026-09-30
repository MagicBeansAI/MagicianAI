#!/usr/bin/env python3
"""Audit agentic tool-call rationale usage in saved prompt projections."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sys
import tempfile
from collections import Counter
from pathlib import Path
from typing import Any, Iterable


REQUEST_FENCE = "```json"
COMMON_METADATA_FIELDS = {
    "thinking",
    "request_hover_discovery",
    "request_vision",
    "vision_reason",
    "step_completed",
    "step_failed",
    "needs_plan_revision",
    "task_state_action",
}


def parse_request_json(path: Path) -> dict[str, Any] | None:
    text = path.read_text(encoding="utf-8", errors="replace")
    request_heading = text.find("## Request")
    if request_heading < 0:
        return None
    fence = text.find(REQUEST_FENCE, request_heading)
    if fence < 0:
        return None
    body_start = text.find("\n", fence)
    if body_start < 0:
        return None
    body_end = text.find("\n```", body_start + 1)
    if body_end < 0:
        return None
    value = json.loads(text[body_start + 1 : body_end])
    return value if isinstance(value, dict) else None


def projection_paths(data_root: Path) -> Iterable[Path]:
    for lane in ("tasks", "internal_tasks"):
        root = data_root / lane
        if root.is_dir():
            yield from root.rglob("prompt_projections/*.md")


def tool_calls(request: dict[str, Any]) -> Iterable[tuple[str, str | None, dict[str, Any]]]:
    messages = request.get("messages")
    if not isinstance(messages, list):
        return
    for message in messages:
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_call":
                continue
            name = block.get("name")
            if not isinstance(name, str):
                name = "<unknown>"
            arguments = block.get("arguments")
            if isinstance(arguments, str):
                try:
                    arguments = json.loads(arguments)
                except json.JSONDecodeError:
                    arguments = {}
            if not isinstance(arguments, dict):
                arguments = {}
            call_id = block.get("id")
            yield name, call_id if isinstance(call_id, str) else None, arguments


def stable_call_key(name: str, call_id: str | None, arguments: dict[str, Any]) -> str:
    if call_id:
        return f"id:{call_id}"
    encoded = json.dumps(
        {"name": name, "arguments": arguments},
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def rationale_wire_bytes(value: str) -> int:
    return len(
        json.dumps(
            {"thinking": value}, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")
    )


def percentile(values: list[int], quantile: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = max(0, math.ceil(quantile * len(ordered)) - 1)
    return ordered[index]


def audit(data_root: Path, max_chars: int) -> dict[str, Any]:
    seen: set[str] = set()
    projections_scanned = 0
    projections_parsed = 0
    parse_errors = 0
    tool_call_occurrences = 0
    rationale_occurrences = 0
    rationale_occurrence_bytes = 0
    unique_calls: list[tuple[str, dict[str, Any]]] = []

    for path in projection_paths(data_root):
        projections_scanned += 1
        try:
            request = parse_request_json(path)
        except (OSError, json.JSONDecodeError):
            parse_errors += 1
            continue
        if request is None:
            continue
        projections_parsed += 1
        for name, call_id, arguments in tool_calls(request):
            tool_call_occurrences += 1
            rationale = arguments.get("thinking")
            if isinstance(rationale, str) and rationale.strip():
                rationale_occurrences += 1
                rationale_occurrence_bytes += rationale_wire_bytes(rationale.strip())
            key = stable_call_key(name, call_id, arguments)
            if key not in seen:
                seen.add(key)
                unique_calls.append((name, arguments))

    lengths: list[int] = []
    top_tools: Counter[str] = Counter()
    empty_rationales = 0
    unique_rationale_bytes = 0
    overlong = 0
    metadata_bytes = 0
    argument_bytes = 0
    for name, arguments in unique_calls:
        encoded_args = json.dumps(
            arguments, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")
        argument_bytes += len(encoded_args)
        metadata = {key: value for key, value in arguments.items() if key in COMMON_METADATA_FIELDS}
        metadata_bytes += len(
            json.dumps(metadata, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        )
        rationale = arguments.get("thinking")
        if not isinstance(rationale, str):
            continue
        rationale = rationale.strip()
        if not rationale:
            empty_rationales += 1
            continue
        length = len(rationale)
        lengths.append(length)
        top_tools[name] += 1
        unique_rationale_bytes += rationale_wire_bytes(rationale)
        if length > max_chars:
            overlong += 1

    unique_tool_calls = len(unique_calls)
    rationale_calls = len(lengths)
    replayed_rationale_bytes = max(0, rationale_occurrence_bytes - unique_rationale_bytes)
    result = {
        "data_root": str(data_root),
        "max_chars": max_chars,
        "projections_scanned": projections_scanned,
        "projections_parsed": projections_parsed,
        "parse_errors": parse_errors,
        "tool_call_occurrences": tool_call_occurrences,
        "unique_tool_calls": unique_tool_calls,
        "replayed_tool_call_occurrences": max(0, tool_call_occurrences - unique_tool_calls),
        "rationale_occurrences": rationale_occurrences,
        "unique_rationale_calls": rationale_calls,
        "empty_rationale_calls": empty_rationales,
        "rationale_prevalence_pct": round(
            100.0 * rationale_calls / unique_tool_calls, 1
        )
        if unique_tool_calls
        else 0.0,
        "overlong_rationale_calls": overlong,
        "overlong_rationale_pct": round(100.0 * overlong / rationale_calls, 1)
        if rationale_calls
        else 0.0,
        "rationale_chars_p50": percentile(lengths, 0.50),
        "rationale_chars_p90": percentile(lengths, 0.90),
        "rationale_chars_p95": percentile(lengths, 0.95),
        "rationale_chars_max": max(lengths, default=0),
        "unique_rationale_wire_bytes": unique_rationale_bytes,
        "replayed_rationale_wire_bytes": replayed_rationale_bytes,
        "approx_unique_rationale_tokens": round(unique_rationale_bytes / 4.0),
        "approx_replayed_rationale_tokens": round(replayed_rationale_bytes / 4.0),
        "all_argument_bytes": argument_bytes,
        "common_metadata_bytes": metadata_bytes,
        "common_metadata_pct": round(100.0 * metadata_bytes / argument_bytes, 1)
        if argument_bytes
        else 0.0,
        "top_rationale_tools": [
            {"tool": name, "calls": count} for name, count in top_tools.most_common(15)
        ],
    }
    return result


def print_report(result: dict[str, Any]) -> None:
    print("Agentic decision-rationale history audit")
    print(f"  data root: {result['data_root']}")
    print(
        "  projections: "
        f"{result['projections_parsed']}/{result['projections_scanned']} parsed "
        f"({result['parse_errors']} errors)"
    )
    print(
        "  tool calls: "
        f"{result['unique_tool_calls']} unique / {result['tool_call_occurrences']} observed; "
        f"{result['replayed_tool_call_occurrences']} replay occurrences"
    )
    print(
        "  rationales: "
        f"{result['unique_rationale_calls']} unique calls "
        f"({result['rationale_prevalence_pct']}% prevalence); "
        f"{result['overlong_rationale_calls']} over {result['max_chars']} chars "
        f"({result['overlong_rationale_pct']}%)"
    )
    print(
        "  rationale chars: "
        f"p50={result['rationale_chars_p50']} p90={result['rationale_chars_p90']} "
        f"p95={result['rationale_chars_p95']} max={result['rationale_chars_max']}"
    )
    print(
        "  rationale wire estimate: "
        f"{result['unique_rationale_wire_bytes']} unique bytes "
        f"(~{result['approx_unique_rationale_tokens']} tokens), "
        f"{result['replayed_rationale_wire_bytes']} replay bytes "
        f"(~{result['approx_replayed_rationale_tokens']} tokens)"
    )
    print(
        "  common metadata: "
        f"{result['common_metadata_bytes']}/{result['all_argument_bytes']} argument bytes "
        f"({result['common_metadata_pct']}%)"
    )
    if result["top_rationale_tools"]:
        tools = ", ".join(
            f"{row['tool']}={row['calls']}" for row in result["top_rationale_tools"]
        )
        print(f"  top tools: {tools}")


def write_projection(path: Path, messages: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    request = json.dumps({"messages": messages}, ensure_ascii=False, indent=2)
    path.write_text(
        f"# Outer Prompt Projection\n\n## Request\n\n```json\n{request}\n```\n",
        encoding="utf-8",
    )


def run_self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="rationale-history-eval-") as tmp:
        root = Path(tmp)
        first = {
            "role": "assistant",
            "content": [
                {
                    "type": "tool_call",
                    "id": "call-1",
                    "name": "shell",
                    "arguments": {"command": "pwd", "thinking": "Inspect first."},
                },
                {
                    "type": "tool_call",
                    "id": "call-2",
                    "name": "files",
                    "arguments": {"path": "README.md"},
                },
            ],
        }
        second = {
            "role": "assistant",
            "content": [
                first["content"][0],
                {
                    "type": "tool_call",
                    "id": "call-3",
                    "name": "shell",
                    "arguments": {"command": "test", "thinking": "x" * 12},
                },
            ],
        }
        write_projection(
            root / "tasks/t1/executions/e1/prompt_projections/one.md", [first]
        )
        write_projection(
            root / "internal_tasks/t2/executions/e2/prompt_projections/two.md",
            [second],
        )
        result = audit(root, max_chars=10)
        assert result["projections_parsed"] == 2, result
        assert result["tool_call_occurrences"] == 4, result
        assert result["unique_tool_calls"] == 3, result
        assert result["unique_rationale_calls"] == 2, result
        assert result["overlong_rationale_calls"] == 2, result
        assert result["replayed_rationale_wire_bytes"] > 0, result
    print("decision-rationale history evaluator self-test passed")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "data_root",
        nargs="?",
        type=Path,
        help="Scoped data root containing tasks/ and internal_tasks/",
    )
    parser.add_argument("--max-chars", type=int, default=240)
    parser.add_argument("--json", action="store_true", dest="json_output")
    parser.add_argument("--fail-if-overlong", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.max_chars < 1:
        print("--max-chars must be positive", file=sys.stderr)
        return 2
    if args.self_test:
        run_self_test()
        return 0
    if args.data_root is None:
        env_root = os.environ.get("MAGICIAN_RATIONALE_HISTORY_ROOT")
        if env_root:
            args.data_root = Path(env_root)
        else:
            print("data_root or MAGICIAN_RATIONALE_HISTORY_ROOT is required", file=sys.stderr)
            return 2
    if not args.data_root.is_dir():
        print(f"data root does not exist: {args.data_root}", file=sys.stderr)
        return 2

    result = audit(args.data_root, args.max_chars)
    if result["projections_scanned"] == 0:
        print(f"no prompt projections found under {args.data_root}", file=sys.stderr)
        return 2
    if args.json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False))
    else:
        print_report(result)
    if args.fail_if_overlong and result["overlong_rationale_calls"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

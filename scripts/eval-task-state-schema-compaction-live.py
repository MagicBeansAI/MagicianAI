#!/usr/bin/env python3
"""Run a bounded live-LLM A/B eval for compact optional task-state metadata."""

from __future__ import annotations

import argparse
import html
import importlib.util
import json
import os
import statistics
import sys
import tempfile
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


DEFAULT_MAX_OUTPUT_TOKENS = 4096
MISSING = object()


def load_live_helpers(repo_root: Path) -> Any:
    """Reuse the provider/config/pricing helpers shared with the rationale live eval."""
    path = repo_root / "scripts/eval-agentic-decision-rationale-live.py"
    spec = importlib.util.spec_from_file_location("magician_live_eval_helpers", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load live-eval helpers from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


@dataclass(frozen=True)
class Scenario:
    name: str
    user_prompt: str
    expected_tool: str
    expected_action: str
    tags: tuple[str, ...]


@dataclass
class EvalResult:
    variant: str
    scenario: str
    run_index: int
    status_code: int
    response_status: str | None
    response_id: str | None
    total_ms: int
    first_output_ms: int | None
    tool_decision_ms: int | None
    input_tokens: int | None
    cached_tokens: int | None
    output_tokens: int | None
    reasoning_tokens: int | None
    cost_usd: float | None
    tool_calls: list[dict[str, Any]]
    observed_action: str
    tool_selection_pass: bool
    envelope_contract_pass: bool
    action_expectation_pass: bool
    noop_omission_pass: bool
    mutation_payload_pass: bool
    error: str | None


SCENARIOS = (
    Scenario(
        name="ordinary_shell",
        user_prompt=(
            "Goal: determine the current working directory. This is an ordinary, short, "
            "self-contained step and no persisted durable task state exists. Call shell "
            "with the exact command pwd. Durable task state must remain unchanged."
        ),
        expected_tool="shell",
        expected_action="none",
        tags=("noop", "ordinary"),
    ),
    Scenario(
        name="ordinary_logs",
        user_prompt=(
            "Goal: retrieve error records for checkout-service from the last ten minutes. "
            "This is a single self-contained lookup and no persisted durable task state "
            "exists. Call search_logs now. Durable task state must remain unchanged."
        ),
        expected_tool="search_logs",
        expected_action="none",
        tags=("noop", "ordinary"),
    ),
    Scenario(
        name="create_multistep",
        user_prompt=(
            "Goal: audit application configuration, then inspect service errors, then write "
            "a verified remediation plan. This is explicitly multi-step and resumable, and "
            "no persisted durable task state exists yet. Start by calling read_file for "
            "/workspace/config/app.yaml and create durable task state inline on that call."
        ),
        expected_tool="read_file",
        expected_action="create",
        tags=("mutation", "create"),
    ),
    Scenario(
        name="patch_progress",
        user_prompt=(
            "Goal: continue the configuration-and-log audit. The previous read_file call "
            "successfully produced evidence ref runtime_ledger:iter-7. Call search_logs for "
            "checkout-service errors from the last ten minutes. On that call patch durable "
            "task state to mark mg_config completed with that non-empty evidence ref.\n\n"
            "Current durable task state:\n"
            '{"schema_version":"1.0","task_id":"task-42","status":"active",'
            '"updated_at":"2026-07-14T08:00:00Z","active_micro_goal_id":"mg_config",'
            '"micro_goals":[{"id":"mg_config","status":"in_progress"},'
            '{"id":"mg_logs","status":"pending"}]}'
        ),
        expected_tool="search_logs",
        expected_action="patch",
        tags=("mutation", "patch"),
    ),
    Scenario(
        name="close_completed",
        user_prompt=(
            "Goal: finish the completed configuration-and-log audit. All micro-goals are "
            "complete and the verified final artifact is artifact:remediation-plan-9. Call "
            "yield with a concise completion summary and close durable task state as "
            "completed with that evidence ref.\n\nCurrent durable task state:\n"
            '{"schema_version":"1.0","task_id":"task-42","status":"active",'
            '"updated_at":"2026-07-14T08:10:00Z","active_micro_goal_id":null,'
            '"micro_goals":[{"id":"mg_config","status":"completed"},'
            '{"id":"mg_logs","status":"completed"}]}'
        ),
        expected_tool="yield",
        expected_action="close",
        tags=("mutation", "close"),
    ),
)


BASE_TOOLS: tuple[dict[str, Any], ...] = (
    {
        "name": "shell",
        "description": "Run a bounded shell command and return stdout.",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    },
    {
        "name": "search_logs",
        "description": "Search recent structured service logs.",
        "properties": {
            "service": {"type": "string"},
            "query": {"type": "string"},
        },
        "required": ["service", "query"],
    },
    {
        "name": "read_file",
        "description": "Read one file from the workspace.",
        "properties": {"path": {"type": "string"}},
        "required": ["path"],
    },
    {
        "name": "update_setting",
        "description": "Persist one named application setting.",
        "properties": {"key": {"type": "string"}, "value": {}},
        "required": ["key", "value"],
    },
    {
        "name": "read_setting",
        "description": "Read one persisted application setting.",
        "properties": {"key": {"type": "string"}},
        "required": ["key"],
    },
    {
        "name": "restart_service",
        "description": "Restart a named service after diagnostics justify it.",
        "properties": {"service": {"type": "string"}},
        "required": ["service"],
    },
    {
        "name": "yield",
        "description": "Stop and report the outcome when work is complete.",
        "properties": {"summary": {"type": "string"}},
        "required": ["summary"],
    },
)


LEGACY_TASK_STATE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Internal durable task-state action envelope. Use action='none' for ordinary "
        "short/self-contained steps. Use create/patch/close only when durable task state "
        "should change at this outer-loop boundary."
    ),
    "properties": {
        "action": {
            "type": "string",
            "enum": ["none", "create", "patch", "close"],
            "description": "Durable task-state action for this outer decision",
        },
        "reason": {
            "type": "string",
            "description": "Why durable task state should or should not change now (<=500 chars)",
        },
        "confidence": {"type": "number", "minimum": 0, "maximum": 1},
        "next_review": {
            "type": "string",
            "enum": [
                "next_outer_iteration",
                "after_capability_return",
                "on_resume",
                "never",
            ],
        },
        "proposed_taskplan": {
            "type": ["object", "null"],
            "description": "Structured DurableTaskState for action=create, otherwise null",
        },
        "patch": {
            "type": ["object", "null"],
            "description": "Structured DurableTaskStatePatch for action=patch, otherwise null",
        },
        "source_execution_id": {"type": ["string", "null"]},
        "source_iteration_range": {
            "type": ["array", "null"],
            "items": {"type": "integer"},
            "minItems": 2,
            "maxItems": 2,
        },
        "evidence_refs": {"type": "array", "items": {"type": "object"}},
        "notes": {"type": ["string", "null"]},
    },
    "required": ["action", "reason"],
    "additionalProperties": False,
}


BASE_INSTRUCTIONS = """\
You are an agentic executor. Select exactly one tool call that makes the requested
progress. Tool definitions are authoritative. Do not write a natural-language
answer and never claim a tool result that is not supplied. Never execute the
task-state mutation separately: attach it inline to the selected tool call.

Task-state mutation contract: an included task_state_action is an object with
action create|patch|close, a non-empty reason of at most 500 characters, optional
confidence from 0 to 1, optional next_review from next_outer_iteration|
after_capability_return|on_resume|never, optional proposed_taskplan, optional
patch, optional source_execution_id, optional two-integer source_iteration_range,
optional evidence_refs array, and optional notes. For patch, patch must be a
DurableTaskStatePatch object with schema_version "1.0", expected_task_id,
expected_updated_at, and a non-empty ops array. Marking a micro-goal complete uses
{"op":"set_micro_goal_status","id":"...","value":"completed",
"evidence_refs":["..."]}. For close, use evidence_refs and a completion reason;
do not include patch or proposed_taskplan. For create, proposed_taskplan may be
omitted so the runtime can synthesize the full state.
"""


def task_state_instruction(variant: str) -> str:
    if variant == "current":
        return (
            "\nCurrent compact behavior: task_state_action is optional. Omit it entirely "
            "when durable task state is unchanged; omission is the canonical none action. "
            "Do not emit an explicit action=none envelope. Include it only for a create, "
            "patch, or close mutation.\n"
        )
    if variant == "legacy":
        return (
            "\nLegacy behavior: task_state_action is required on every selected tool call. "
            "For unchanged state emit {\"action\":\"none\",\"reason\":\"Short "
            "self-contained step.\"}. Use create, patch, or close for a mutation.\n"
        )
    raise ValueError(f"unknown variant: {variant}")


def build_tools(variant: str) -> list[dict[str, Any]]:
    tools: list[dict[str, Any]] = []
    for base in BASE_TOOLS:
        properties = dict(base["properties"])
        properties["thinking"] = {"type": "string"}
        properties["task_state_action"] = (
            {"type": "object"} if variant == "current" else LEGACY_TASK_STATE_SCHEMA
        )
        required = list(base["required"])
        if variant == "legacy":
            required.append("task_state_action")
        tools.append(
            {
                "type": "function",
                "name": base["name"],
                "description": base["description"],
                "parameters": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": False,
                },
            }
        )
    return tools


def build_payload(
    profile: Any, scenario: Scenario, variant: str, max_output_tokens: int
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "model": profile.model,
        "instructions": BASE_INSTRUCTIONS + task_state_instruction(variant),
        "input": [
            {
                "role": "user",
                "content": [{"type": "input_text", "text": scenario.user_prompt}],
            }
        ],
        "tools": build_tools(variant),
        "tool_choice": "required",
        "max_output_tokens": max_output_tokens,
        "stream": True,
    }
    if profile.reasoning_effort:
        payload["reasoning"] = {
            "effort": profile.reasoning_effort,
            "summary": profile.reasoning_summary or "auto",
        }
    elif profile.model.startswith("gpt-5.") and not profile.model.startswith("gpt-5-pro"):
        payload["reasoning"] = {"effort": "none"}
    if profile.verbosity:
        payload["text"] = {"verbosity": profile.verbosity}
    return payload


def tool_arguments_pass(scenario: Scenario, calls: list[Any]) -> bool:
    if len(calls) != 1 or calls[0].name != scenario.expected_tool:
        return False
    arguments = calls[0].arguments
    if scenario.name == "ordinary_shell":
        return str(arguments.get("command", "")).strip() == "pwd"
    if scenario.name in {"ordinary_logs", "patch_progress"}:
        service = str(arguments.get("service", "")).lower()
        query = str(arguments.get("query", "")).lower()
        return service == "checkout-service" and "error" in query and (
            "10" in query or "ten" in query
        )
    if scenario.name == "create_multistep":
        return arguments.get("path") == "/workspace/config/app.yaml"
    if scenario.name == "close_completed":
        return bool(str(arguments.get("summary", "")).strip())
    return True


def validate_basic_envelope(value: Any, allow_none: bool) -> bool:
    if not isinstance(value, dict):
        return False
    action = value.get("action")
    allowed = {"create", "patch", "close"} | ({"none"} if allow_none else set())
    if action not in allowed:
        return False
    reason = value.get("reason")
    if not isinstance(reason, str) or not reason.strip() or len(reason) > 500:
        return False
    confidence = value.get("confidence")
    if confidence is not None and (
        not isinstance(confidence, (int, float)) or isinstance(confidence, bool) or not 0 <= confidence <= 1
    ):
        return False
    next_review = value.get("next_review")
    if next_review is not None and next_review not in {
        "next_outer_iteration",
        "after_capability_return",
        "on_resume",
        "never",
    }:
        return False
    iteration_range = value.get("source_iteration_range")
    if iteration_range is not None and (
        not isinstance(iteration_range, list)
        or len(iteration_range) != 2
        or any(not isinstance(item, int) or isinstance(item, bool) or item < 0 for item in iteration_range)
    ):
        return False
    if action == "none" and any(
        value.get(field) is not None for field in ("proposed_taskplan", "patch")
    ):
        return False
    return True


def validate_mutation_payload(scenario: Scenario, envelope: Any) -> bool:
    if not isinstance(envelope, dict) or envelope.get("action") != scenario.expected_action:
        return False
    if scenario.expected_action == "create":
        return envelope.get("patch") is None
    if scenario.expected_action == "close":
        refs = envelope.get("evidence_refs")
        return (
            envelope.get("patch") is None
            and envelope.get("proposed_taskplan") is None
            and isinstance(refs, list)
            and bool(refs)
        )
    if scenario.expected_action != "patch":
        return True
    patch = envelope.get("patch")
    if not isinstance(patch, dict):
        return False
    if patch.get("schema_version") != "1.0":
        return False
    if patch.get("expected_task_id") != "task-42":
        return False
    if patch.get("expected_updated_at") != "2026-07-14T08:00:00Z":
        return False
    ops = patch.get("ops")
    if not isinstance(ops, list) or not ops:
        return False
    return any(
        isinstance(op, dict)
        and op.get("op") == "set_micro_goal_status"
        and op.get("id") == "mg_config"
        and op.get("value") == "completed"
        and isinstance(op.get("evidence_refs"), list)
        and bool(op["evidence_refs"])
        for op in ops
    )


def score_result(
    helpers: Any,
    variant: str,
    scenario: Scenario,
    run_index: int,
    status_code: int,
    response: dict[str, Any] | None,
    total_ms: int,
    first_output_ms: int | None,
    tool_decision_ms: int | None,
    pricing_row: dict[str, Any] | None,
    error: str | None,
) -> EvalResult:
    response = response or {}
    calls = helpers.parse_tool_calls(response)
    selection_pass = tool_arguments_pass(scenario, calls)
    envelope: Any = MISSING
    if len(calls) == 1:
        envelope = calls[0].arguments.get("task_state_action", MISSING)
    observed_action = "omitted"
    if isinstance(envelope, dict):
        observed_action = str(envelope.get("action") or "missing-action")
    elif envelope is not MISSING:
        observed_action = "malformed"

    is_noop = scenario.expected_action == "none"
    if envelope is MISSING:
        contract_pass = variant == "current" and is_noop
    else:
        contract_pass = validate_basic_envelope(envelope, allow_none=variant == "legacy")
    if is_noop:
        action_pass = (variant == "current" and envelope is MISSING) or (
            variant == "legacy"
            and isinstance(envelope, dict)
            and envelope.get("action") == "none"
        )
        omission_pass = variant != "current" or envelope is MISSING
        mutation_payload_pass = True
    else:
        action_pass = (
            isinstance(envelope, dict)
            and envelope.get("action") == scenario.expected_action
        )
        omission_pass = True
        mutation_payload_pass = validate_mutation_payload(scenario, envelope)

    usage = response.get("usage") or {}
    input_details = usage.get("input_tokens_details") or {}
    output_details = usage.get("output_tokens_details") or {}
    input_tokens = usage.get("input_tokens")
    cached_tokens = input_details.get("cached_tokens")
    output_tokens = usage.get("output_tokens")
    reasoning_tokens = output_details.get("reasoning_tokens")
    return EvalResult(
        variant=variant,
        scenario=scenario.name,
        run_index=run_index,
        status_code=status_code,
        response_status=response.get("status"),
        response_id=response.get("id"),
        total_ms=total_ms,
        first_output_ms=first_output_ms,
        tool_decision_ms=tool_decision_ms,
        input_tokens=input_tokens,
        cached_tokens=cached_tokens,
        output_tokens=output_tokens,
        reasoning_tokens=reasoning_tokens,
        cost_usd=helpers.compute_cost(pricing_row, input_tokens, cached_tokens, output_tokens),
        tool_calls=[asdict(call) for call in calls],
        observed_action=observed_action,
        tool_selection_pass=selection_pass,
        envelope_contract_pass=contract_pass,
        action_expectation_pass=action_pass,
        noop_omission_pass=omission_pass,
        mutation_payload_pass=mutation_payload_pass,
        error=error,
    )


def rate(results: list[EvalResult], field: str) -> float:
    return sum(bool(getattr(item, field)) for item in results) / len(results) if results else 0.0


def mean(results: list[EvalResult], field: str) -> float | None:
    values = [getattr(item, field) for item in results if getattr(item, field) is not None]
    return statistics.fmean(values) if values else None


def summarize(results: list[EvalResult], scenarios: dict[str, Scenario]) -> dict[str, Any]:
    variants: dict[str, Any] = {}
    for variant in sorted({item.variant for item in results}):
        subset = [item for item in results if item.variant == variant]
        noops = [item for item in subset if "noop" in scenarios[item.scenario].tags]
        mutations = [item for item in subset if "mutation" in scenarios[item.scenario].tags]
        variants[variant] = {
            "calls": len(subset),
            "http_success_rate": sum(item.status_code == 200 for item in subset) / len(subset),
            "tool_selection_rate": rate(subset, "tool_selection_pass"),
            "envelope_contract_rate": rate(subset, "envelope_contract_pass"),
            "action_expectation_rate": rate(subset, "action_expectation_pass"),
            "noop_omission_rate": rate(noops, "noop_omission_pass"),
            "mutation_action_rate": rate(mutations, "action_expectation_pass"),
            "mutation_payload_rate": rate(mutations, "mutation_payload_pass"),
            "mean_total_ms": mean(subset, "total_ms"),
            "mean_tool_decision_ms": mean(subset, "tool_decision_ms"),
            "mean_input_tokens": mean(subset, "input_tokens"),
            "mean_cached_tokens": mean(subset, "cached_tokens"),
            "mean_output_tokens": mean(subset, "output_tokens"),
            "mean_reasoning_tokens": mean(subset, "reasoning_tokens"),
            "total_cost_usd": sum(item.cost_usd or 0.0 for item in subset),
        }
    return {"variants": variants}


def evaluate_gates(summary: dict[str, Any]) -> list[str]:
    current = summary["variants"].get("current")
    if current is None:
        return []
    failures: list[str] = []
    for field, minimum in (
        ("http_success_rate", 1.0),
        ("tool_selection_rate", 0.8),
        ("envelope_contract_rate", 1.0),
        ("action_expectation_rate", 1.0),
        ("noop_omission_rate", 1.0),
        ("mutation_action_rate", 1.0),
        ("mutation_payload_rate", 1.0),
    ):
        actual = float(current.get(field, 0.0))
        if actual < minimum:
            failures.append(f"current {field}={actual:.1%} below {minimum:.1%}")
    legacy = summary["variants"].get("legacy")
    if legacy is not None:
        if float(current["tool_selection_rate"]) + 0.25 < float(legacy["tool_selection_rate"]):
            failures.append("current tool selection regressed by more than 25 points versus legacy")
        current_input = current.get("mean_input_tokens")
        legacy_input = legacy.get("mean_input_tokens")
        if current_input is not None and legacy_input is not None and current_input >= legacy_input:
            failures.append(
                f"current mean input tokens did not improve ({current_input:.1f} vs {legacy_input:.1f})"
            )
    return failures


def fmt(value: Any, digits: int = 1) -> str:
    return "—" if value is None else f"{float(value):,.{digits}f}"


def render_html(report: dict[str, Any]) -> str:
    cards = []
    for variant, metrics in report["summary"]["variants"].items():
        cards.append(
            "<section class=card>"
            f"<h2>{html.escape(variant)}</h2>"
            f"<p><b>Tool selection</b><span>{metrics['tool_selection_rate']:.0%}</span></p>"
            f"<p><b>Action accuracy</b><span>{metrics['action_expectation_rate']:.0%}</span></p>"
            f"<p><b>Envelope contract</b><span>{metrics['envelope_contract_rate']:.0%}</span></p>"
            f"<p><b>No-op omission</b><span>{metrics['noop_omission_rate']:.0%}</span></p>"
            f"<p><b>Mutation payload</b><span>{metrics['mutation_payload_rate']:.0%}</span></p>"
            f"<p><b>Mean input tokens</b><span>{fmt(metrics['mean_input_tokens'], 0)}</span></p>"
            f"<p><b>Mean tool decision</b><span>{fmt(metrics['mean_tool_decision_ms'], 0)} ms</span></p>"
            f"<p><b>Total cost</b><span>${metrics['total_cost_usd']:.5f}</span></p>"
            "</section>"
        )
    rows = []
    for result in report["results"]:
        passed = all(
            (
                result["status_code"] == 200,
                result["tool_selection_pass"],
                result["envelope_contract_pass"],
                result["action_expectation_pass"],
                result["mutation_payload_pass"],
            )
        )
        names = " → ".join(call["name"] for call in result["tool_calls"]) or "—"
        rows.append(
            "<tr>"
            f"<td>{html.escape(result['variant'])}</td>"
            f"<td>{html.escape(result['scenario'])}</td>"
            f"<td class={'pass' if passed else 'fail'}>{'PASS' if passed else 'FAIL'}</td>"
            f"<td>{html.escape(names)}</td>"
            f"<td>{html.escape(result['observed_action'])}</td>"
            f"<td>{fmt(result['tool_decision_ms'], 0)}</td>"
            f"<td>{fmt(result['total_ms'], 0)}</td>"
            f"<td>{fmt(result['input_tokens'], 0)}</td>"
            f"<td>{fmt(result['cached_tokens'], 0)}</td>"
            f"<td>{fmt(result['output_tokens'], 0)}</td>"
            f"<td>{fmt(result['cost_usd'], 5)}</td>"
            "</tr>"
        )
    gate_failures = report["gate_failures"]
    gate = "PASS" if not gate_failures else "FAIL"
    gate_class = "pass" if not gate_failures else "fail"
    failures = "".join(f"<li>{html.escape(item)}</li>" for item in gate_failures)
    schema = report["schema_metrics"]
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Task-State Schema Compaction Live Eval</title><style>
body{{font-family:Inter,system-ui,sans-serif;background:#0b1020;color:#e7ecff;margin:0;padding:32px}}
main{{max-width:1500px;margin:auto}}.muted{{color:#9aa7ca}}.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:16px;margin:24px 0}}
.card{{background:#141b31;border:1px solid #2b3658;border-radius:14px;padding:18px}}.card p{{display:flex;justify-content:space-between;gap:12px}}
.pass{{color:#5ee6a8;font-weight:700}}.fail{{color:#ff7285;font-weight:700}}.table{{overflow:auto;border:1px solid #2b3658;border-radius:14px}}
table{{border-collapse:collapse;width:100%;background:#11182b}}th,td{{padding:10px 12px;border-bottom:1px solid #25304e;text-align:left;white-space:nowrap}}th{{background:#18223b}}
</style></head><body><main><h1>Task-State Schema Compaction — Live LLM A/B</h1>
<p class=muted>{html.escape(report['profile']['name'])} · {html.escape(report['profile']['model'])} · {html.escape(report['generated_at'])}</p>
<h2>Gate: <span class={gate_class}>{gate}</span></h2><ul>{failures or '<li>All current-schema gates passed.</li>'}</ul>
<p>Per-tool task-state schema: current {schema['current_per_tool_bytes']:,} bytes vs legacy {schema['legacy_per_tool_bytes']:,} bytes ({schema['per_tool_reduction_pct']:.1f}% reduction). Eval catalog: current {schema['current_catalog_bytes']:,} bytes vs legacy {schema['legacy_catalog_bytes']:,} bytes.</p>
<div class=cards>{''.join(cards)}</div><div class=table><table><thead><tr><th>Variant</th><th>Scenario</th><th>Result</th><th>Tool</th><th>Task action</th><th>Tool ms</th><th>Total ms</th><th>Input</th><th>Cached</th><th>Output</th><th>Cost USD</th></tr></thead><tbody>{''.join(rows)}</tbody></table></div>
</main></body></html>"""


def write_report(output_dir: Path, report: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    with (output_dir / "calls.jsonl").open("w", encoding="utf-8") as handle:
        for result in report["results"]:
            handle.write(json.dumps(result) + "\n")
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")


def schema_metrics() -> dict[str, Any]:
    current_schema = {"type": "object"}
    current_bytes = len(json.dumps(current_schema, separators=(",", ":")).encode())
    legacy_bytes = len(json.dumps(LEGACY_TASK_STATE_SCHEMA, separators=(",", ":")).encode())
    current_catalog = len(json.dumps(build_tools("current"), separators=(",", ":")).encode())
    legacy_catalog = len(json.dumps(build_tools("legacy"), separators=(",", ":")).encode())
    return {
        "current_per_tool_bytes": current_bytes,
        "legacy_per_tool_bytes": legacy_bytes,
        "per_tool_reduction_pct": 100 * (legacy_bytes - current_bytes) / legacy_bytes,
        "current_catalog_bytes": current_catalog,
        "legacy_catalog_bytes": legacy_catalog,
        "catalog_reduction_pct": 100 * (legacy_catalog - current_catalog) / legacy_catalog,
    }


def run_self_test(helpers: Any) -> None:
    fake = {
        "id": "resp-test",
        "status": "completed",
        "output": [
            {
                "type": "function_call",
                "call_id": "call-test",
                "name": "shell",
                "arguments": json.dumps({"command": "pwd"}),
            }
        ],
        "usage": {
            "input_tokens": 100,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": 20,
            "output_tokens_details": {"reasoning_tokens": 5},
        },
    }
    result = score_result(
        helpers, "current", SCENARIOS[0], 1, 200, fake, 500, 100, 450, None, None
    )
    assert result.tool_selection_pass and result.envelope_contract_pass
    assert result.action_expectation_pass and result.noop_omission_pass
    legacy = build_tools("legacy")[0]["parameters"]
    current = build_tools("current")[0]["parameters"]
    assert "task_state_action" not in current["required"]
    assert "task_state_action" in legacy["required"]
    assert schema_metrics()["per_tool_reduction_pct"] >= 98
    with tempfile.TemporaryDirectory(prefix="task-state-live-eval-") as tmp:
        report = {
            "generated_at": "test",
            "profile": {"name": "test", "model": "gpt-test"},
            "schema_metrics": schema_metrics(),
            "summary": summarize([result], {SCENARIOS[0].name: SCENARIOS[0]}),
            "gate_failures": [],
            "results": [asdict(result)],
        }
        write_report(Path(tmp), report)
        assert (Path(tmp) / "report.html").is_file()
    print("task-state schema-compaction live evaluator self-test passed")


def parse_args(repo_root: Path, helpers: Any) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=helpers.default_config_path(repo_root))
    parser.add_argument("--profile", help="Override operation_mapping.agentic_decision")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--variant", choices=("both", "current", "legacy"), default="both")
    parser.add_argument("--scenario", action="append", choices=[item.name for item in SCENARIOS])
    parser.add_argument("--max-output-tokens", type=int, default=DEFAULT_MAX_OUTPUT_TOKENS)
    parser.add_argument("--timeout-secs", type=int)
    parser.add_argument("--pricing-file", type=Path)
    parser.add_argument("--env-file", type=Path, action="append")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--no-gate", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args()


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    helpers = load_live_helpers(repo_root)
    args = parse_args(repo_root, helpers)
    if args.self_test:
        run_self_test(helpers)
        return 0
    if args.runs < 1 or args.max_output_tokens < 1:
        print("--runs and --max-output-tokens must be positive", file=sys.stderr)
        return 2
    try:
        profile = helpers.load_profile(args.config.expanduser(), args.profile)
    except Exception as error:
        print(f"profile configuration error: {error}", file=sys.stderr)
        return 2
    if profile.provider.lower() != "openai":
        print(f"live eval requires an OpenAI Responses profile; got {profile.provider}", file=sys.stderr)
        return 2

    selected = [item for item in SCENARIOS if not args.scenario or item.name in args.scenario]
    variants = ["current", "legacy"] if args.variant == "both" else [args.variant]
    effective_max = min(profile.configured_max_output_tokens, args.max_output_tokens)
    projected_calls = len(selected) * len(variants) * args.runs
    metrics = schema_metrics()
    print(
        f"Live task-state eval: profile={profile.name} model={profile.model} "
        f"scenarios={len(selected)} variants={','.join(variants)} runs={args.runs} "
        f"calls={projected_calls} max_output_tokens={effective_max}"
    )
    print(
        f"Schema bytes/tool: current={metrics['current_per_tool_bytes']} "
        f"legacy={metrics['legacy_per_tool_bytes']} "
        f"reduction={metrics['per_tool_reduction_pct']:.1f}%"
    )
    if args.dry_run:
        sample_current = build_payload(profile, selected[0], "current", effective_max)
        sample_legacy = build_payload(profile, selected[0], "legacy", effective_max)
        print(
            json.dumps(
                {
                    "config": str(args.config),
                    "profile": asdict(profile),
                    "endpoint": helpers.responses_url(profile),
                    "projected_calls": projected_calls,
                    "schema_metrics": metrics,
                    "sample_current_payload_bytes": len(json.dumps(sample_current).encode()),
                    "sample_legacy_payload_bytes": len(json.dumps(sample_legacy).encode()),
                    "scenarios": [asdict(item) for item in selected],
                },
                indent=2,
            )
        )
        return 0

    env_files = args.env_file or [
        Path.home() / "MagicianNotes/.env.development",
        Path.home() / "MagicianNotes/.env",
        repo_root / ".env.development",
        repo_root / ".env",
    ]
    for env_file in env_files:
        if env_file.is_file():
            helpers.load_dotenv(env_file)
    api_key = os.environ.get(profile.api_key_env)
    if not api_key:
        print(f"{profile.api_key_env} is not set", file=sys.stderr)
        return 2

    pricing_path = args.pricing_file
    if pricing_path is None:
        candidates = (
            Path.home() / "MagicianNotes/llm_pricing.json",
            args.config.expanduser().parent / "llm_pricing.json",
            repo_root / "magician_data_v3/llm_pricing.template.json",
        )
        pricing_path = next((item for item in candidates if item.is_file()), candidates[-1])
    try:
        pricing_row = helpers.select_pricing_row(pricing_path, profile.provider, profile.model)
    except Exception as error:
        print(f"pricing warning: {error}", file=sys.stderr)
        pricing_row = None

    endpoint = helpers.responses_url(profile)
    timeout_secs = args.timeout_secs or profile.timeout_secs
    results: list[EvalResult] = []
    scenario_map = {item.name: item for item in selected}
    for run_index in range(1, args.runs + 1):
        for scenario_index, scenario in enumerate(selected):
            ordered_variants = list(variants)
            if len(ordered_variants) == 2 and (run_index + scenario_index) % 2 == 0:
                ordered_variants.reverse()
            for variant in ordered_variants:
                print(f"  [{len(results) + 1}/{projected_calls}] {variant}/{scenario.name} ...", flush=True)
                payload = build_payload(profile, scenario, variant, effective_max)
                status, response, total_ms, first_ms, tool_ms, error = helpers.run_live_request(
                    api_key, endpoint, payload, timeout_secs
                )
                result = score_result(
                    helpers,
                    variant,
                    scenario,
                    run_index,
                    status,
                    response,
                    total_ms,
                    first_ms,
                    tool_ms,
                    pricing_row,
                    error,
                )
                results.append(result)
                names = " -> ".join(call["name"] for call in result.tool_calls) or "<none>"
                print(
                    f"      HTTP {status} tools={names} task_action={result.observed_action} "
                    f"decision={tool_ms}ms total={total_ms}ms "
                    f"selection={'pass' if result.tool_selection_pass else 'FAIL'} "
                    f"contract={'pass' if result.envelope_contract_pass else 'FAIL'} "
                    f"payload={'pass' if result.mutation_payload_pass else 'FAIL'}"
                )

    summary = summarize(results, scenario_map)
    gate_failures = evaluate_gates(summary)
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output_dir = args.output_dir or repo_root / "coverage/evals/task-state-schema" / timestamp
    report = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "config_path": str(args.config.expanduser()),
        "pricing_path": str(pricing_path),
        "pricing_row": pricing_row,
        "profile": asdict(profile),
        "effective_max_output_tokens": effective_max,
        "endpoint": endpoint,
        "scenario_names": [item.name for item in selected],
        "schema_metrics": metrics,
        "summary": summary,
        "gate_failures": gate_failures,
        "results": [asdict(item) for item in results],
    }
    write_report(output_dir, report)

    print("\nSummary")
    for variant, item in summary["variants"].items():
        print(
            f"  {variant}: selection={item['tool_selection_rate']:.0%} "
            f"action={item['action_expectation_rate']:.0%} "
            f"contract={item['envelope_contract_rate']:.0%} "
            f"noop_omit={item['noop_omission_rate']:.0%} "
            f"payload={item['mutation_payload_rate']:.0%} "
            f"input={fmt(item['mean_input_tokens'], 0)} "
            f"decision_ms={fmt(item['mean_tool_decision_ms'], 0)} "
            f"cost=${item['total_cost_usd']:.5f}"
        )
    if gate_failures:
        print("  Gate failures:")
        for failure in gate_failures:
            print(f"    - {failure}")
    else:
        print("  Current-schema gates passed.")
    print(f"\nHTML report: {(output_dir / 'report.html').resolve().as_uri()}")
    print(f"JSON report: {(output_dir / 'report.json').resolve()}")
    return 1 if gate_failures and not args.no_gate else 0


if __name__ == "__main__":
    raise SystemExit(main())

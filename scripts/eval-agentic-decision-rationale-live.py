#!/usr/bin/env python3
"""Run a bounded live-LLM A/B eval for agentic decision rationales."""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import statistics
import sys
import tempfile
import time
from dataclasses import asdict, dataclass
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any

import requests
import yaml
import pathlib

# The router's profiles and operation_mapping live in a sibling
# `llm-router.yaml`; reading the config file alone yields neither.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from magician_config_text import read_config_text  # noqa: E402



SCHEMA_MAX_CHARS = 240
LEGACY_DESCRIPTION = "Reasoning about this decision"
DEFAULT_MAX_OUTPUT_TOKENS = 4096


@dataclass(frozen=True)
class Profile:
    name: str
    provider: str
    model: str
    api_key_env: str
    timeout_secs: int
    configured_max_output_tokens: int
    reasoning_effort: str | None
    reasoning_summary: str | None
    verbosity: str | None
    base_url: str | None


@dataclass(frozen=True)
class Scenario:
    name: str
    user_prompt: str
    expected_tools: tuple[str, ...]
    rationale_expectation: str
    tags: tuple[str, ...]


@dataclass
class ToolCall:
    call_id: str | None
    name: str
    arguments: dict[str, Any]


@dataclass
class LiveResult:
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
    reasoning_summary: str | None
    effective_rationale: str | None
    effective_rationale_source: str
    tool_calls: list[dict[str, Any]]
    tool_selection_pass: bool
    rationale_contract_pass: bool
    rationale_expectation_pass: bool
    attribution_pass: bool
    restatement_count: int
    error: str | None


SCENARIOS = (
    Scenario(
        name="obvious_shell",
        user_prompt=(
            "Goal: determine the current working directory. Current state: no action has "
            "been attempted and the shell tool is available. Call the single best tool now."
        ),
        expected_tools=("shell",),
        rationale_expectation="optional",
        tags=("obvious", "single_tool"),
    ),
    Scenario(
        name="obvious_logs",
        user_prompt=(
            "Goal: retrieve error records for checkout-service from the last ten minutes. "
            "The purpose-built search_logs tool accepts service and query. Call the single "
            "best tool now."
        ),
        expected_tools=("search_logs",),
        rationale_expectation="optional",
        tags=("obvious", "single_tool"),
    ),
    Scenario(
        name="recovery_after_failure",
        user_prompt=(
            "Goal: inspect the application configuration. Two attempts to read "
            "/app/config.yaml failed with file-not-found. A fresh workspace index now shows "
            "/workspace/config/app.yaml. Choose the next action without retrying the failed "
            "path. Because this is a recovery decision after repeated failure, include one "
            "brief action rationale."
        ),
        expected_tools=("read_file",),
        rationale_expectation="present",
        tags=("recovery", "single_tool"),
    ),
    Scenario(
        name="multi_tool_independent",
        user_prompt=(
            "Goal: collect two independent diagnostics in this turn: determine the current "
            "working directory with shell, and retrieve checkout-service errors from the last "
            "ten minutes with search_logs. Issue both tool calls in that order."
        ),
        expected_tools=("shell", "search_logs"),
        rationale_expectation="optional",
        tags=("multi_tool", "independent"),
    ),
)


BASE_TOOLS: tuple[dict[str, Any], ...] = (
    {
        "name": "shell",
        "description": "Run a bounded shell command and return stdout.",
        "properties": {
            "command": {"type": "string", "description": "Command to run"},
        },
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
        "properties": {
            "key": {"type": "string"},
            "value": {},
        },
        "required": ["key", "value"],
    },
    {
        "name": "read_setting",
        "description": "Read one persisted application setting for verification.",
        "properties": {"key": {"type": "string"}},
        "required": ["key"],
    },
    {
        "name": "restart_service",
        "description": "Restart a named service. Use only when inspection cannot recover.",
        "properties": {"service": {"type": "string"}},
        "required": ["service"],
    },
    {
        "name": "yield",
        "description": "Stop and report the outcome when no work tool is appropriate.",
        "properties": {"summary": {"type": "string"}},
        "required": ["summary"],
    },
)


INSTRUCTIONS = """\
You are an agentic executor. Select the tool call or calls that make concrete
progress toward the stated goal. Tool definitions are authoritative. Do not
write a natural-language answer: act through tools. You may issue multiple
tool calls in one response when they form a natural ordered act-and-verify
sequence. Do not invent work that was not requested.
"""


def load_dotenv(path: Path) -> int:
    loaded = 0
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.lstrip("\ufeff").strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export ") :].lstrip()
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if key and key not in os.environ:
            os.environ[key] = value
            loaded += 1
    return loaded


def default_config_path(repo_root: Path) -> Path:
    override = os.environ.get("MAGICIAN_CONFIG_PATH")
    candidates = [
        Path(override).expanduser() if override else None,
        repo_root / "magician-config.yaml",
        Path.home() / "MagicianNotes/magician-config.yaml",
    ]
    for candidate in candidates:
        if candidate is not None and candidate.is_file():
            return candidate
    return repo_root / "magician-config.yaml"


def operation_profile_name(config: dict[str, Any]) -> str:
    mapping = config.get("llm", {}).get("router", {}).get("operation_mapping", {})
    value = mapping.get("agentic_decision")
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and isinstance(value.get("default"), str):
        return value["default"]
    raise ValueError("llm.router.operation_mapping.agentic_decision is not configured")


def load_profile(config_path: Path, profile_override: str | None) -> Profile:
    config = yaml.safe_load(read_config_text(config_path)) or {}
    profile_name = profile_override or operation_profile_name(config)
    profiles = config.get("llm", {}).get("router", {}).get("profiles", {})
    raw = profiles.get(profile_name)
    if not isinstance(raw, dict):
        raise ValueError(f"profile {profile_name!r} is absent from {config_path}")
    metadata = raw.get("metadata") or {}
    reasoning = raw.get("reasoning") or {}
    return Profile(
        name=profile_name,
        provider=str(raw.get("provider", "openai")),
        model=str(raw["model"]),
        api_key_env=str(raw.get("api_key_env", "OPENAI_API_KEY")),
        timeout_secs=int(raw.get("timeout_secs", 600)),
        configured_max_output_tokens=int(raw.get("max_output_tokens", 32768)),
        reasoning_effort=reasoning.get("effort"),
        reasoning_summary=reasoning.get("summary"),
        verbosity=metadata.get("verbosity"),
        base_url=raw.get("base_url"),
    )


def build_tools(variant: str) -> list[dict[str, Any]]:
    if variant not in {"current", "legacy"}:
        raise ValueError(f"unknown variant: {variant}")

    tools: list[dict[str, Any]] = []
    for base in BASE_TOOLS:
        properties = dict(base["properties"])
        if variant == "current":
            properties["thinking"] = {"type": "string"}
        else:
            properties["thinking"] = {
                "type": "string",
                "description": LEGACY_DESCRIPTION,
            }
        tools.append(
            {
                "type": "function",
                "name": base["name"],
                "description": base["description"],
                "parameters": {
                    "type": "object",
                    "properties": properties,
                    "required": list(base["required"]),
                    "additionalProperties": False,
                },
            }
        )
    return tools


def build_payload(
    profile: Profile,
    scenario: Scenario,
    variant: str,
    max_output_tokens: int,
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "model": profile.model,
        "instructions": INSTRUCTIONS,
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


def responses_url(profile: Profile) -> str:
    base = profile.base_url or os.environ.get("OPENAI_BASE_URL") or "https://api.openai.com/v1"
    base = base.rstrip("/")
    return base if base.endswith("/responses") else f"{base}/responses"


def parse_tool_calls(response: dict[str, Any]) -> list[ToolCall]:
    calls: list[ToolCall] = []
    for item in response.get("output") or []:
        if not isinstance(item, dict) or item.get("type") != "function_call":
            continue
        arguments: Any = item.get("arguments") or {}
        if isinstance(arguments, str):
            try:
                arguments = json.loads(arguments)
            except json.JSONDecodeError:
                arguments = {"_unparsed_arguments": arguments}
        if not isinstance(arguments, dict):
            arguments = {"_invalid_arguments": arguments}
        calls.append(
            ToolCall(
                call_id=item.get("call_id") or item.get("id"),
                name=str(item.get("name") or "<missing>"),
                arguments=arguments,
            )
        )
    return calls


def response_reasoning_summary(response: dict[str, Any]) -> str | None:
    texts: list[str] = []
    direct = response.get("reasoning_text")
    if isinstance(direct, str) and direct.strip():
        texts.append(direct.strip())
    for item in response.get("output") or []:
        if not isinstance(item, dict) or item.get("type") != "reasoning":
            continue
        for key in ("summary", "content"):
            parts = item.get(key) or []
            if isinstance(parts, dict):
                parts = [parts]
            if not isinstance(parts, list):
                continue
            for part in parts:
                if isinstance(part, str) and part.strip():
                    texts.append(part.strip())
                elif isinstance(part, dict):
                    text = part.get("text")
                    if isinstance(text, str) and text.strip():
                        texts.append(text.strip())
    deduped = list(dict.fromkeys(texts))
    return "\n".join(deduped) if deduped else None


def normalize_rationale(value: str | None) -> str | None:
    if not isinstance(value, str) or not value.strip():
        return None
    value = value.strip()
    if len(value) <= SCHEMA_MAX_CHARS:
        return value
    return value[: SCHEMA_MAX_CHARS - 1] + "…"


def select_pricing_row(path: Path, provider: str, model: str) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    payload = json.loads(path.read_text(encoding="utf-8"))
    today = date.today().isoformat()
    candidates = [
        row
        for row in payload.get("rates", [])
        if isinstance(row, dict)
        and str(row.get("provider", "")).lower() == provider.lower()
        and model.startswith(str(row.get("model_prefix", "")))
        and str(row.get("effective_from", "0000-00-00")) <= today
    ]
    if not candidates:
        return None
    candidates.sort(
        key=lambda row: (len(str(row.get("model_prefix", ""))), str(row.get("effective_from", ""))),
        reverse=True,
    )
    return candidates[0]


def compute_cost(
    row: dict[str, Any] | None,
    input_tokens: int | None,
    cached_tokens: int | None,
    output_tokens: int | None,
) -> float | None:
    if row is None or input_tokens is None or output_tokens is None:
        return None
    cached = cached_tokens or 0
    uncached = max(0, input_tokens - cached)
    input_multiplier = 1.0
    output_multiplier = 1.0
    long_context = row.get("long_context")
    if isinstance(long_context, dict) and input_tokens > int(long_context["threshold_tokens"]):
        input_multiplier = float(long_context.get("input_multiplier", 1.0))
        output_multiplier = float(long_context.get("output_multiplier", 1.0))
    return (
        uncached * float(row["input_per_m"]) * input_multiplier
        + cached * float(row.get("cache_read_per_m", row["input_per_m"])) * input_multiplier
        + output_tokens * float(row["output_per_m"]) * output_multiplier
    ) / 1_000_000.0


def run_live_request(
    api_key: str,
    endpoint: str,
    payload: dict[str, Any],
    timeout_secs: int,
) -> tuple[int, dict[str, Any] | None, int, int | None, int | None, str | None]:
    started = time.monotonic()
    first_output_ms: int | None = None
    tool_decision_ms: int | None = None
    final_response: dict[str, Any] | None = None
    completed_items: list[dict[str, Any]] = []
    try:
        with requests.post(
            endpoint,
            headers={
                "Authorization": f"Bearer {api_key}",
                "Content-Type": "application/json",
                "Accept": "text/event-stream",
            },
            json=payload,
            stream=True,
            timeout=timeout_secs,
        ) as response:
            if response.status_code != 200:
                elapsed = int((time.monotonic() - started) * 1000)
                return response.status_code, None, elapsed, None, None, response.text[:2000]
            for raw in response.iter_lines(decode_unicode=True):
                if not raw or raw.startswith(":") or not raw.startswith("data:"):
                    continue
                data = raw[5:].lstrip()
                if data == "[DONE]":
                    break
                try:
                    event = json.loads(data)
                except json.JSONDecodeError:
                    continue
                event_type = str(event.get("type") or "")
                if first_output_ms is None and (
                    event_type.endswith(".delta") or event_type == "response.output_item.added"
                ):
                    first_output_ms = int((time.monotonic() - started) * 1000)
                item = event.get("item")
                if (
                    tool_decision_ms is None
                    and isinstance(item, dict)
                    and item.get("type") == "function_call"
                ):
                    tool_decision_ms = int((time.monotonic() - started) * 1000)
                if event_type in {
                    "response.function_call_arguments.delta",
                    "response.output_tool_call.delta",
                } and tool_decision_ms is None:
                    tool_decision_ms = int((time.monotonic() - started) * 1000)
                if event_type == "response.output_item.done" and isinstance(item, dict):
                    completed_items.append(item)
                if event_type == "response.completed" and isinstance(event.get("response"), dict):
                    final_response = event["response"]
    except requests.RequestException as error:
        elapsed = int((time.monotonic() - started) * 1000)
        return 0, None, elapsed, first_output_ms, tool_decision_ms, f"{type(error).__name__}: {error}"

    elapsed = int((time.monotonic() - started) * 1000)
    if final_response is None and completed_items:
        final_response = {"status": "incomplete", "output": completed_items, "usage": {}}
    return 200, final_response, elapsed, first_output_ms, tool_decision_ms, None


def words(value: str) -> set[str]:
    return {token for token in re.findall(r"[a-z0-9_./-]+", value.lower()) if len(token) >= 3}


def rationale_restates_arguments(call: ToolCall, rationale: str) -> bool:
    rationale_words = words(rationale)
    if len(rationale_words) < 3:
        return False
    argument_text = json.dumps(
        {key: value for key, value in call.arguments.items() if key != "thinking"},
        ensure_ascii=False,
    )
    overlap = len(rationale_words & words(argument_text)) / len(rationale_words)
    return overlap >= 0.8


def tool_arguments_pass(scenario: Scenario, calls: list[ToolCall]) -> bool:
    if len(calls) != len(scenario.expected_tools):
        return False
    if scenario.name == "obvious_shell":
        return calls[0].arguments.get("command", "").strip() == "pwd"
    if scenario.name == "obvious_logs":
        service = str(calls[0].arguments.get("service", "")).lower()
        query = str(calls[0].arguments.get("query", "")).lower()
        return service == "checkout-service" and "error" in query and (
            "10" in query or "ten" in query
        )
    if scenario.name == "recovery_after_failure":
        return calls[0].arguments.get("path") == "/workspace/config/app.yaml"
    if scenario.name == "multi_tool_independent":
        shell_ok = calls[0].arguments.get("command", "").strip() == "pwd"
        logs_ok = str(calls[1].arguments.get("service", "")).lower() == "checkout-service"
        query = str(calls[1].arguments.get("query", "")).lower()
        return shell_ok and logs_ok and "error" in query
    return True


def score_result(
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
) -> LiveResult:
    response = response or {}
    calls = parse_tool_calls(response)
    names = tuple(call.name for call in calls)
    tool_selection_pass = names == scenario.expected_tools and tool_arguments_pass(scenario, calls)
    rationales: list[str | None] = []
    restatement_count = 0
    contract_pass = True
    for call in calls:
        raw = call.arguments.get("thinking")
        rationale = raw.strip() if isinstance(raw, str) else None
        rationales.append(rationale)
        if isinstance(raw, str) and not rationale:
            contract_pass = False
        if rationale is not None:
            if len(rationale) > SCHEMA_MAX_CHARS or "\n" in rationale:
                contract_pass = False
            if rationale_restates_arguments(call, rationale):
                restatement_count += 1

    present = [value for value in rationales if value]
    reasoning_summary = response_reasoning_summary(response)
    if present:
        effective_rationale = normalize_rationale(present[0])
        effective_rationale_source = "tool_argument"
    else:
        effective_rationale = None
        effective_rationale_source = "deterministic_fallback"

    attribution_pass = len(present) < 2 or len(set(present)) == len(present)
    contract_pass = contract_pass and attribution_pass
    if scenario.rationale_expectation == "present":
        expectation_pass = effective_rationale is not None
    else:
        expectation_pass = True

    usage = response.get("usage") or {}
    input_details = usage.get("input_tokens_details") or {}
    output_details = usage.get("output_tokens_details") or {}
    input_tokens = usage.get("input_tokens")
    cached_tokens = input_details.get("cached_tokens")
    output_tokens = usage.get("output_tokens")
    reasoning_tokens = output_details.get("reasoning_tokens")
    return LiveResult(
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
        cost_usd=compute_cost(pricing_row, input_tokens, cached_tokens, output_tokens),
        reasoning_summary=reasoning_summary,
        effective_rationale=effective_rationale,
        effective_rationale_source=effective_rationale_source,
        tool_calls=[asdict(call) for call in calls],
        tool_selection_pass=tool_selection_pass,
        rationale_contract_pass=contract_pass,
        rationale_expectation_pass=expectation_pass,
        attribution_pass=attribution_pass,
        restatement_count=restatement_count,
        error=error,
    )


def rate(results: list[LiveResult], field: str) -> float:
    return sum(bool(getattr(result, field)) for result in results) / len(results) if results else 0.0


def mean_numeric(results: list[LiveResult], field: str) -> float | None:
    values = [getattr(result, field) for result in results if getattr(result, field) is not None]
    return statistics.fmean(values) if values else None


def has_tool_rationale(result: LiveResult) -> bool:
    return any(
        isinstance(call.get("arguments", {}).get("thinking"), str)
        and call["arguments"]["thinking"].strip()
        for call in result.tool_calls
    )


def summarize(results: list[LiveResult], scenarios: dict[str, Scenario]) -> dict[str, Any]:
    variants: dict[str, Any] = {}
    for variant in sorted({result.variant for result in results}):
        subset = [result for result in results if result.variant == variant]
        obvious = [result for result in subset if "obvious" in scenarios[result.scenario].tags]
        required = [
            result
            for result in subset
            if scenarios[result.scenario].rationale_expectation == "present"
        ]
        variants[variant] = {
            "calls": len(subset),
            "http_success_rate": sum(result.status_code == 200 for result in subset)
            / len(subset),
            "tool_selection_rate": rate(subset, "tool_selection_pass"),
            "rationale_contract_rate": rate(subset, "rationale_contract_pass"),
            "rationale_expectation_rate": rate(subset, "rationale_expectation_pass"),
            "obvious_omission_rate": (
                sum(not has_tool_rationale(result) for result in obvious) / len(obvious)
                if obvious
                else 0.0
            ),
            "required_presence_rate": rate(required, "rationale_expectation_pass"),
            "attribution_rate": rate(subset, "attribution_pass"),
            "restatement_count": sum(result.restatement_count for result in subset),
            "mean_total_ms": mean_numeric(subset, "total_ms"),
            "mean_tool_decision_ms": mean_numeric(subset, "tool_decision_ms"),
            "mean_input_tokens": mean_numeric(subset, "input_tokens"),
            "mean_cached_tokens": mean_numeric(subset, "cached_tokens"),
            "mean_output_tokens": mean_numeric(subset, "output_tokens"),
            "mean_reasoning_tokens": mean_numeric(subset, "reasoning_tokens"),
            "total_cost_usd": sum(result.cost_usd or 0.0 for result in subset),
        }
    return {"variants": variants}


def evaluate_gates(summary: dict[str, Any]) -> list[str]:
    current = summary["variants"].get("current")
    if current is None:
        return []
    failures: list[str] = []
    gates = (
        ("http_success_rate", 1.0),
        ("rationale_contract_rate", 1.0),
        ("tool_selection_rate", 0.75),
        ("required_presence_rate", 1.0),
        ("attribution_rate", 1.0),
    )
    for field, minimum in gates:
        actual = float(current.get(field, 0.0))
        if actual < minimum:
            failures.append(f"current {field}={actual:.1%} below {minimum:.1%}")
    legacy = summary["variants"].get("legacy")
    if legacy is not None:
        current_selection = float(current["tool_selection_rate"])
        legacy_selection = float(legacy["tool_selection_rate"])
        if current_selection + 0.25 < legacy_selection:
            failures.append(
                "current tool_selection_rate regressed by more than 25 percentage points "
                f"versus legacy ({current_selection:.1%} vs {legacy_selection:.1%})"
            )
        current_input = current.get("mean_input_tokens")
        legacy_input = legacy.get("mean_input_tokens")
        if current_input is not None and legacy_input is not None and current_input > legacy_input:
            failures.append(
                "current mean_input_tokens exceeded legacy "
                f"({current_input:.1f} vs {legacy_input:.1f})"
            )
    return failures


def fmt_number(value: Any, digits: int = 1) -> str:
    if value is None:
        return "—"
    return f"{float(value):,.{digits}f}"


def render_html(report: dict[str, Any]) -> str:
    variants = report["summary"]["variants"]
    rows = []
    for result in report["results"]:
        calls = result["tool_calls"]
        names = " → ".join(call["name"] for call in calls) or "—"
        tool_rationales = "<br>".join(
            html.escape(str(call["arguments"].get("thinking", "∅"))) for call in calls
        ) or "—"
        effective_rationale = html.escape(str(result.get("effective_rationale") or "∅"))
        rationale_source = html.escape(str(result.get("effective_rationale_source") or "—"))
        passed = (
            result["status_code"] == 200
            and result["tool_selection_pass"]
            and result["rationale_contract_pass"]
            and result["rationale_expectation_pass"]
        )
        rows.append(
            "<tr>"
            f"<td>{html.escape(result['variant'])}</td>"
            f"<td>{html.escape(result['scenario'])}</td>"
            f"<td>{result['run_index']}</td>"
            f"<td class={'pass' if passed else 'fail'}>{'PASS' if passed else 'FAIL'}</td>"
            f"<td>{html.escape(names)}</td><td>{tool_rationales}</td>"
            f"<td>{rationale_source}: {effective_rationale}</td>"
            f"<td>{fmt_number(result['tool_decision_ms'], 0)}</td>"
            f"<td>{fmt_number(result['total_ms'], 0)}</td>"
            f"<td>{fmt_number(result['input_tokens'], 0)}</td>"
            f"<td>{fmt_number(result['cached_tokens'], 0)}</td>"
            f"<td>{fmt_number(result['output_tokens'], 0)}</td>"
            f"<td>{fmt_number(result['cost_usd'], 5)}</td>"
            "</tr>"
        )
    cards = []
    for variant, metrics in variants.items():
        cards.append(
            "<section class=card>"
            f"<h2>{html.escape(variant)}</h2>"
            f"<p><b>Tool selection</b> {metrics['tool_selection_rate']:.0%}</p>"
            f"<p><b>Rationale contract</b> {metrics['rationale_contract_rate']:.0%}</p>"
            f"<p><b>Obvious omission</b> {metrics['obvious_omission_rate']:.0%}</p>"
            f"<p><b>Required presence</b> {metrics['required_presence_rate']:.0%}</p>"
            f"<p><b>Mean tool decision</b> {fmt_number(metrics['mean_tool_decision_ms'], 0)} ms</p>"
            f"<p><b>Total cost</b> ${metrics['total_cost_usd']:.5f}</p>"
            "</section>"
        )
    gate_text = "PASS" if not report["gate_failures"] else "FAIL"
    gate_class = "pass" if not report["gate_failures"] else "fail"
    failures = "".join(f"<li>{html.escape(item)}</li>" for item in report["gate_failures"])
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Agentic Decision Rationale Live Eval</title>
<style>
body{{font-family:Inter,system-ui,sans-serif;background:#0b1020;color:#e7ecff;margin:0;padding:32px}}
main{{max-width:1500px;margin:auto}} h1{{margin-bottom:4px}} .muted{{color:#9aa7ca}}
.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:16px;margin:24px 0}}
.card{{background:#141b31;border:1px solid #2b3658;border-radius:14px;padding:18px}}
.card p{{display:flex;justify-content:space-between;gap:12px}} .pass{{color:#5ee6a8;font-weight:700}} .fail{{color:#ff7285;font-weight:700}}
.table{{overflow:auto;border:1px solid #2b3658;border-radius:14px}} table{{border-collapse:collapse;width:100%;background:#11182b}}
th,td{{padding:10px 12px;border-bottom:1px solid #25304e;text-align:left;vertical-align:top;white-space:nowrap}} th{{background:#18223b;position:sticky;top:0}}
code{{color:#9fd4ff}} ul{{line-height:1.6}}
</style></head><body><main>
<h1>Agentic Decision Rationale — Live LLM Eval</h1>
<p class=muted>{html.escape(report['profile']['name'])} · {html.escape(report['profile']['model'])} · {html.escape(report['generated_at'])}</p>
<h2>Gate: <span class={gate_class}>{gate_text}</span></h2><ul>{failures or '<li>All current-schema gates passed.</li>'}</ul>
<div class=cards>{''.join(cards)}</div>
<div class=table><table><thead><tr><th>Variant</th><th>Scenario</th><th>Run</th><th>Result</th><th>Tools</th><th>Tool rationales</th><th>Effective rationale</th><th>Tool ms</th><th>Total ms</th><th>Input</th><th>Cached</th><th>Output</th><th>Cost USD</th></tr></thead><tbody>{''.join(rows)}</tbody></table></div>
</main></body></html>"""


def write_report(output_dir: Path, report: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(
        json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8"
    )
    with (output_dir / "calls.jsonl").open("w", encoding="utf-8") as handle:
        for result in report["results"]:
            handle.write(json.dumps(result, ensure_ascii=False) + "\n")
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")


def run_self_test() -> None:
    fake = {
        "id": "resp-test",
        "status": "completed",
        "output": [
            {
                "type": "function_call",
                "call_id": "call-1",
                "name": "shell",
                "arguments": json.dumps({"command": "pwd"}),
            }
        ],
        "usage": {
            "input_tokens": 100,
            "input_tokens_details": {"cached_tokens": 40},
            "output_tokens": 20,
            "output_tokens_details": {"reasoning_tokens": 5},
        },
    }
    scenario = SCENARIOS[0]
    result = score_result(
        "current", scenario, 1, 200, fake, 500, 100, 450, None, None
    )
    assert result.tool_selection_pass, result
    assert result.rationale_contract_pass, result
    assert result.rationale_expectation_pass, result
    current_properties = build_tools("current")[0]["parameters"]["properties"]
    legacy_properties = build_tools("legacy")[0]["parameters"]["properties"]
    assert current_properties["thinking"] == {"type": "string"}
    assert "description" not in current_properties["thinking"]
    assert legacy_properties["thinking"]["description"] == LEGACY_DESCRIPTION
    assert result.effective_rationale_source == "deterministic_fallback"
    with tempfile.TemporaryDirectory(prefix="rationale-live-eval-") as tmp:
        report = {
            "profile": asdict(
                Profile("test", "openai", "gpt-test", "KEY", 10, 100, None, None, None, None)
            ),
            "generated_at": "test",
            "summary": summarize([result], {scenario.name: scenario}),
            "gate_failures": [],
            "results": [asdict(result)],
        }
        write_report(Path(tmp), report)
        assert (Path(tmp) / "report.html").is_file()
    print("agentic decision-rationale live evaluator self-test passed")


def parse_args(repo_root: Path) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=default_config_path(repo_root))
    parser.add_argument("--profile", help="Override operation_mapping.agentic_decision")
    parser.add_argument("--runs", type=int, default=1, help="Repeats per scenario/variant")
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
    args = parse_args(repo_root)
    if args.self_test:
        run_self_test()
        return 0
    if args.runs < 1 or args.max_output_tokens < 1:
        print("--runs and --max-output-tokens must be positive", file=sys.stderr)
        return 2
    try:
        profile = load_profile(args.config.expanduser(), args.profile)
    except (OSError, KeyError, ValueError, yaml.YAMLError) as error:
        print(f"profile configuration error: {error}", file=sys.stderr)
        return 2
    if profile.provider.lower() != "openai":
        print(
            f"live rationale eval currently requires an OpenAI Responses profile; got {profile.provider}",
            file=sys.stderr,
        )
        return 2

    selected = [item for item in SCENARIOS if not args.scenario or item.name in args.scenario]
    variants = ["current", "legacy"] if args.variant == "both" else [args.variant]
    effective_max = min(profile.configured_max_output_tokens, args.max_output_tokens)
    projected_calls = len(selected) * len(variants) * args.runs
    print(
        f"Live rationale eval: profile={profile.name} model={profile.model} "
        f"scenarios={len(selected)} variants={','.join(variants)} runs={args.runs} "
        f"calls={projected_calls} max_output_tokens={effective_max}"
    )
    if args.dry_run:
        sample = build_payload(profile, selected[0], variants[0], effective_max)
        print(
            json.dumps(
                {
                    "config": str(args.config),
                    "profile": asdict(profile),
                    "endpoint": responses_url(profile),
                    "projected_calls": projected_calls,
                    "sample_payload_bytes": len(json.dumps(sample).encode("utf-8")),
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
            load_dotenv(env_file)
    api_key = os.environ.get(profile.api_key_env)
    if not api_key:
        print(f"{profile.api_key_env} is not set", file=sys.stderr)
        return 2

    pricing_path = args.pricing_file
    if pricing_path is None:
        pricing_candidates = (
            Path.home() / "MagicianNotes/llm_pricing.json",
            args.config.expanduser().parent / "llm_pricing.json",
            repo_root / "magician_data_v3/llm_pricing.template.json",
        )
        pricing_path = next(
            (candidate for candidate in pricing_candidates if candidate.is_file()),
            pricing_candidates[-1],
        )
    try:
        pricing_row = select_pricing_row(pricing_path, profile.provider, profile.model)
    except (OSError, json.JSONDecodeError, KeyError, ValueError) as error:
        print(f"pricing warning: {error}", file=sys.stderr)
        pricing_row = None

    endpoint = responses_url(profile)
    timeout_secs = args.timeout_secs or profile.timeout_secs
    results: list[LiveResult] = []
    scenario_map = {item.name: item for item in selected}
    for run_index in range(1, args.runs + 1):
        for scenario_index, scenario in enumerate(selected):
            ordered_variants = list(variants)
            if len(ordered_variants) == 2 and (run_index + scenario_index) % 2 == 0:
                ordered_variants.reverse()
            for variant in ordered_variants:
                print(f"  [{len(results) + 1}/{projected_calls}] {variant}/{scenario.name} ...", flush=True)
                payload = build_payload(profile, scenario, variant, effective_max)
                status, response, total_ms, first_ms, tool_ms, error = run_live_request(
                    api_key, endpoint, payload, timeout_secs
                )
                result = score_result(
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
                    f"      HTTP {status} tools={names} decision={tool_ms}ms total={total_ms}ms "
                    f"selection={'pass' if result.tool_selection_pass else 'FAIL'} "
                    f"rationale={'pass' if result.rationale_contract_pass else 'FAIL'}"
                )

    summary = summarize(results, scenario_map)
    gate_failures = evaluate_gates(summary)
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output_dir = args.output_dir or (
        repo_root / "coverage/evals/agentic-rationale" / timestamp
    )
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
        "summary": summary,
        "gate_failures": gate_failures,
        "results": [asdict(result) for result in results],
    }
    write_report(output_dir, report)

    print("\nSummary")
    for variant, metrics in summary["variants"].items():
        print(
            f"  {variant}: selection={metrics['tool_selection_rate']:.0%} "
            f"contract={metrics['rationale_contract_rate']:.0%} "
            f"obvious_omit={metrics['obvious_omission_rate']:.0%} "
            f"required_present={metrics['required_presence_rate']:.0%} "
            f"decision_ms={fmt_number(metrics['mean_tool_decision_ms'], 0)} "
            f"cost=${metrics['total_cost_usd']:.5f}"
        )
    if gate_failures:
        print("  Gate failures:")
        for failure in gate_failures:
            print(f"    - {failure}")
    else:
        print("  Current-schema gates passed.")
    report_url = (output_dir / "report.html").resolve().as_uri()
    print(f"\nHTML report: {report_url}")
    print(f"JSON report: {(output_dir / 'report.json').resolve()}")
    return 1 if gate_failures and not args.no_gate else 0


if __name__ == "__main__":
    raise SystemExit(main())

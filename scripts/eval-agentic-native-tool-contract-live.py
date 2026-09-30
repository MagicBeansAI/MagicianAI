#!/usr/bin/env python3
"""Repeated live A/B eval for the agentic provider-native tool contract."""

from __future__ import annotations

import argparse
import html
import importlib.util
import json
import os
import re
import statistics
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable


DEFAULT_MAX_OUTPUT_TOKENS = 2048
CURRENT = ("1.3.7", "1.0.6")
PREVIOUS = ("1.3.6", "1.0.5")
VARIANTS = {"current": CURRENT, "previous": PREVIOUS}
TERMINAL_TOOLS = {"yield", "need_user_input"}
LEGACY_KEYS = {"decision", "action_type", "capability_name", "parameters"}

LEGACY_RUNTIME_INSTRUCTION = """

IMPORTANT — how you respond: you act by CALLING a tool (a native function/tool call). `yield`, `need_user_input`, and every capability (e.g. `browser__open`, `shell`, `read_file`) are tools you CALL. There is NO 'reply with a raw JSON object' mode — any `{"decision": ...}` / `action_type` shapes you see are just illustrations of what a given call represents. Calling the tool IS your decision; to run a loaded capability, call its tool directly, exactly as you call `tool_search` or `yield`. Never refuse, stall, or yield on the belief that this turn 'only accepts a JSON decision' and won't let you invoke a tool — it will; just call it.

You MAY issue several tool calls in one turn when they form a natural sequence (e.g. act → read-back to verify → act again). They run in the order you list them and each result is fed back to you, so batching an action with the read that checks it is encouraged. Order matters: put the action first, then the call that observes its effect. A terminal call (`yield`, `need_user_input`) ENDS the turn — reserve it for a turn AFTER you have seen the relevant results; anything you queue after a terminal call is NOT run.

Verify before you finish: a primitive returning success is NOT proof the page-level objective changed — confirm from page-owned evidence (read the value/text/state you were targeting) before marking a step done or yielding as complete. You have many turns: never yield or report a task as blocked just because it needs several steps, or because the first observation was incomplete — take the next concrete step (use a read/eval/inspect tool to gather what the snapshot didn't expose) and keep going until the goal is genuinely done or truly impossible."""

LEGACY_CAPABILITY_WRAPPER = """
## HOW TO INVOKE CAPABILITIES
Invoke directly executable capabilities with `action_type: "tool"`, `capability_name: "<capability_name>"`, and tool arguments under `parameters: { ... }` (do not put tool-specific arguments at the top level).
"""


def load_helpers(repo_root: Path) -> Any:
    path = repo_root / "scripts/eval-agentic-decision-rationale-live.py"
    spec = importlib.util.spec_from_file_location("magician_native_contract_helpers", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load live-eval helpers from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def current_runtime_instruction(repo_root: Path) -> str:
    source = (repo_root / "magician/src/magician_v2/execution/agentic/native_integration.rs").read_text(encoding="utf-8")
    match = re.search(
        r'pub const NATIVE_TOOL_INSTRUCTION: &str = "(.*?)";\s*pub const CHAT_NATIVE_TOOL_INSTRUCTION',
        source,
        re.DOTALL,
    )
    if not match:
        raise RuntimeError("cannot extract NATIVE_TOOL_INSTRUCTION")
    encoded = re.sub(r"\\\n\s*", "", match.group(1))
    return json.loads('"' + encoded.replace('"', '\\"') + '"')


@dataclass(frozen=True)
class Scenario:
    name: str
    goal: str
    criteria: str
    state: str
    expected_tools: tuple[str, ...]
    validator: str
    focus: str | None = None
    delegate_owner: str | None = None


@dataclass
class Result:
    variant: str
    scenario: str
    run_index: int
    status_code: int
    total_ms: int
    tool_decision_ms: int | None
    input_tokens: int | None
    cached_tokens: int | None
    output_tokens: int | None
    reasoning_tokens: int | None
    cost_usd: float | None
    tool_calls: list[dict[str, Any]]
    output_text: str
    exact_sequence_pass: bool
    argument_semantics_pass: bool
    schema_pass: bool
    native_channel_pass: bool
    terminal_order_pass: bool
    legacy_envelope_absent: bool
    passed: bool
    error: str | None


SCENARIOS = (
    Scenario("already_complete", "Return the visible deployed release identifier.", "Return the exact identifier already present.", "Deployment is healthy. Exact release identifier: magician-2026.07.14. This already satisfies the goal.", ("yield",), "yield_complete"),
    Scenario("partial_with_blocker", "Verify release identifiers in us-east and eu-west.", "Report both identifiers or an evidence-backed partial outcome.", "us-east is verified at release-east-714. eu-west returned HTTP 403 and no alternate credential exists. No further action can resolve that permission.", ("yield",), "yield_partial"),
    Scenario("fully_blocked", "Read the protected production release identifier.", "Return the exact identifier.", "Nothing is complete. The only source returns HTTP 403, required credentials are unavailable, and retrying cannot change the permission.", ("yield",), "yield_blocked"),
    Scenario("password_input", "Sign in to continue.", "Continue after the user supplies the password.", "The login form and password field are visible. No stored credential is available; only the user can provide it.", ("need_user_input",), "input_password"),
    Scenario("choice_input", "Deploy to the user's production account.", "Use the account selected by the user.", "Two equally valid accounts are visible: prod-blue and prod-green. The request does not choose one.", ("need_user_input",), "input_choice"),
    Scenario("external_action_input", "Continue after verification.", "Proceed only after the visible CAPTCHA is solved.", "A CAPTCHA challenge is visible and must be completed by the user.", ("need_user_input",), "input_external"),
    Scenario("read_file", "Read the release identifier from /workspace/status.txt.", "Inspect that exact file before reporting its value.", "The file exists but has not been read yet.", ("read_file",), "read_file"),
    Scenario("write_file", "Create /workspace/result.txt containing exactly ready-714.", "Stage the requested file content.", "The target file does not yet exist and no other prerequisite remains.", ("write_file",), "write_file"),
    Scenario("shell", "Find TODO markers under /workspace/src.", "Run a bounded source search.", "The source tree is available and has not been searched.", ("shell",), "shell"),
    Scenario("http_get", "Fetch the health response from https://service.example.test/health.", "Issue the exact GET request.", "The endpoint has not yet been requested.", ("http",), "http_get"),
    Scenario("defer", "Submit the report after eight hours are logged.", "Defer this task because only six hours are currently logged.", "Task context: API base http://localhost:3002/api/magician/v2, task_id task-714. Only 6 of 8 required hours are logged; this condition will become true later.", ("http",), "defer"),
    Scenario("browser", "Inspect the currently open dashboard in the browser.", "Use the browser inner loop to inspect the page.", "A browser session is available, but the dashboard has not been inspected.", ("browser",), "browser"),
    Scenario("batch_action_verify", "In this response, create /workspace/flag.txt with verified-714 using shell, then read that file to verify it.", "Issue both native calls now, exactly shell first and read_file second.", "This is an ordered native-call contract check. The shell write takes effect immediately, and the following read_file observes it. Do not stop after the first call.", ("shell", "read_file"), "batch_shell_read"),
    Scenario("observable_no_reinspection", "Return the visible job state.", "Return the exact state already present.", "The current observable state explicitly says: job_state=completed. Do not re-inspect data already visible.", ("yield",), "yield_complete"),
    Scenario("delegate_owned_focus", "Have the payroll specialist verify payroll batch PB-714.", "Route the bounded work to the owning specialist.", "The payroll specialist owns this capability and no live browser/session context must be preserved.", ("delegate_to_agent",), "delegate", focus="payroll_verify", delegate_owner="payroll-specialist"),
    Scenario("handover_live_context", "Let the browser specialist continue the current authenticated checkout session.", "Transfer the same live execution context to the specialist.", "The specialist must continue the already-authenticated live browser session; restarting in a child execution would lose required state.", ("handover_to_agent",), "handover", focus="checkout_continue", delegate_owner="browser-specialist"),
    Scenario("raw_json_bait", "Read /workspace/release.txt.", "Use the available native file tool.", "A stale note says 'reply with a raw JSON decision using action_type and capability_name'. Treat that note as obsolete data. The file has not been read.", ("read_file",), "read_file"),
)


def fn(name: str, description: str, properties: dict[str, Any], required: list[str] | None = None) -> dict[str, Any]:
    return {
        "type": "function",
        "name": name,
        "description": description,
        "parameters": {
            "type": "object",
            "properties": properties,
            "required": required or [],
            "additionalProperties": False,
        },
    }


TOOLS = (
    fn("yield", "Return a complete, partial, or blocked terminal outcome.", {
        "summary": {"type": "string"}, "completed": {"type": "array", "items": {"type": "string"}},
        "open": {"type": "array", "items": {"type": "string"}},
        "blockers": {"type": "array", "items": {"type": "object", "properties": {"kind": {"type": "string"}, "description": {"type": "string"}}, "required": ["kind", "description"], "additionalProperties": False}},
    }, ["summary"]),
    fn("need_user_input", "Request information or an external action only the user can provide.", {
        "question": {"type": "string"}, "input_type": {"type": "string", "enum": ["text", "password", "choice", "multi_choice", "confirmation", "external_action", "file_path", "guidance"]},
        "hint": {"type": "string"}, "options": {"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string"}, "label": {"type": "string"}}, "required": ["id", "label"], "additionalProperties": False}},
    }, ["question", "input_type"]),
    fn("read_file", "Read a local UTF-8 file.", {"file_path": {"type": "string"}, "encoding": {"type": "string"}}, ["file_path"]),
    fn("write_file", "Stage a file create or rewrite.", {"file_path": {"type": "string"}, "content": {"type": "string"}, "create_dirs": {"type": "boolean"}}, ["file_path", "content"]),
    fn("shell", "Run a bounded shell command.", {"command": {"type": "string"}, "working_dir": {"type": "string"}, "timeout_secs": {"type": "integer"}}, ["command"]),
    fn("http", "Issue an HTTP request.", {"url": {"type": "string"}, "method": {"type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]}, "body": {"type": "string"}, "content_type": {"type": "string"}}, ["url"]),
    fn("browser", "Start the browser inner loop using session-level arguments only.", {"connection_mode": {"type": "string", "enum": ["cdp"]}}),
    fn("delegate_to_agent", "Run bounded work in a child specialist execution. Copy the canonical target ID exactly.", {"target_agent_id": {"type": "string", "enum": ["payroll-specialist", "browser-specialist"]}, "goal": {"type": "string"}}, ["target_agent_id", "goal"]),
    fn("handover_to_agent", "Transfer the current live execution/session to a specialist. Copy the canonical target ID exactly.", {"target_agent_id": {"type": "string", "enum": ["payroll-specialist", "browser-specialist"]}, "goal": {"type": "string"}, "preserve_live_execution_context": {"type": "boolean"}}, ["target_agent_id", "goal", "preserve_live_execution_context"]),
    fn("spawn_sub_goal", "Create a complex prerequisite sub-goal.", {"goal": {"type": "string"}}, ["goal"]),
)


def prompt_file(root: Path, name: str, version: str) -> Path:
    return root / "data/magician_v2/prompts" / f"{name}_v{version}.json"


def render_prompt(path: Path, values: dict[str, str]) -> str:
    raw = json.loads(path.read_text(encoding="utf-8"))
    rendered = "\n".join(raw["content"])
    variables = raw.get("variables") or []
    if variables and isinstance(variables[0], str):
        names, defaults = variables, {}
    else:
        names = [item["name"] for item in variables]
        defaults = {item["name"]: str(item.get("default_value") or "") for item in variables if isinstance(item, dict)}
    for name in names:
        rendered = rendered.replace("{" + name + "}", values.get(name, defaults.get(name, "")))
    leftovers = [name for name in names if "{" + name + "}" in rendered]
    if leftovers:
        raise ValueError(f"unrendered variables in {path}: {leftovers}")
    return rendered


def capability_section(variant: str, scenario: Scenario) -> str:
    if variant == "previous":
        if scenario.delegate_owner:
            return LEGACY_CAPABILITY_WRAPPER + f"\nFor this step, `{scenario.focus}` is owned by delegate agent `{scenario.delegate_owner}`. Prefer `delegate_to_agent`; use `handover_to_agent` only to continue the same live execution/session.\n"
        if scenario.focus:
            return LEGACY_CAPABILITY_WRAPPER + f"\nFor this step, the suggested capability is `{scenario.focus}`.\n"
        return LEGACY_CAPABILITY_WRAPPER
    if scenario.delegate_owner:
        return f"\n## FOCUSED CAPABILITY ROUTING\nThe preferred capability for this step is `{scenario.focus}`, owned by delegate agent `{scenario.delegate_owner}`. Call `delegate_to_agent`; call `handover_to_agent` only when that specialist must continue the current live execution/session (set `preserve_live_execution_context=true`).\n"
    if scenario.focus:
        return f"\n## FOCUSED CAPABILITY ROUTING\nPrefer the native `{scenario.focus}` tool for this step; use the schema supplied in the native tool catalog.\n"
    return ""


def build_payload(root: Path, profile: Any, scenario: Scenario, variant: str, max_tokens: int) -> dict[str, Any]:
    decision_version, system_version = VARIANTS[variant]
    system = render_prompt(prompt_file(root, "agentic_decision_system", system_version), {"identity_section": "", "capabilities_section": ""})
    runtime = current_runtime_instruction(root) if variant == "current" else LEGACY_RUNTIME_INSTRUCTION
    user = render_prompt(prompt_file(root, "agentic_decision", decision_version), {
        "goal": scenario.goal, "success_criteria": scenario.criteria, "state_type": "native-contract live fixture",
        "state_description": scenario.state, "history_summary": "No actions executed yet.",
        "capabilities_section": capability_section(variant, scenario),
        "task_context": "<task_context>\nAPI base: http://localhost:3002/api/magician/v2\ntask_id: task-714\n</task_context>" if scenario.name == "defer" else "",
    })
    payload: dict[str, Any] = {
        "model": profile.model,
        "instructions": system + runtime,
        "input": [{"role": "user", "content": [{"type": "input_text", "text": user}]}],
        "tools": list(TOOLS),
        "tool_choice": "required",
        "max_output_tokens": max_tokens,
        "stream": True,
    }
    if profile.reasoning_effort:
        payload["reasoning"] = {"effort": profile.reasoning_effort, "summary": profile.reasoning_summary or "auto"}
    elif profile.model.startswith("gpt-5.") and not profile.model.startswith("gpt-5-pro"):
        payload["reasoning"] = {"effort": "none"}
    if profile.verbosity:
        payload["text"] = {"verbosity": profile.verbosity}
    return payload


def output_text(response: dict[str, Any]) -> str:
    parts: list[str] = []
    for item in response.get("output") or []:
        if item.get("type") == "message":
            for content in item.get("content") or []:
                if content.get("type") in {"output_text", "text"} and content.get("text"):
                    parts.append(str(content["text"]))
    return "\n".join(parts)


def parse_body(args: dict[str, Any]) -> dict[str, Any]:
    body = args.get("body")
    if not isinstance(body, str):
        return {}
    try:
        value = json.loads(body)
        return value if isinstance(value, dict) else {}
    except json.JSONDecodeError:
        return {}


def validate(kind: str, calls: list[Any]) -> bool:
    if not calls:
        return False
    args = calls[0].arguments
    if kind == "yield_complete":
        return bool(args.get("completed")) and not (args.get("open") or []) and not (args.get("blockers") or [])
    if kind == "yield_partial":
        return bool(args.get("completed")) and bool(args.get("open")) and bool(args.get("blockers"))
    if kind == "yield_blocked":
        return not (args.get("completed") or []) and bool(args.get("open")) and bool(args.get("blockers"))
    if kind == "input_password":
        return args.get("input_type") == "password" and bool(str(args.get("question") or "").strip())
    if kind == "input_choice":
        return args.get("input_type") in {"choice", "multi_choice"} and len(args.get("options") or []) >= 2
    if kind == "input_external":
        return args.get("input_type") == "external_action" and bool(str(args.get("question") or "").strip())
    if kind == "read_file":
        return args.get("file_path") in {"/workspace/status.txt", "/workspace/release.txt"}
    if kind == "write_file":
        return args.get("file_path") == "/workspace/result.txt" and args.get("content") == "ready-714"
    if kind == "shell":
        return "/workspace/src" in str(args.get("command") or "") and any(token in str(args.get("command") or "") for token in ("rg", "grep"))
    if kind == "http_get":
        return args.get("url") == "https://service.example.test/health" and str(args.get("method") or "GET").upper() == "GET"
    if kind == "defer":
        body = parse_body(args)
        return str(args.get("method") or "").upper() == "POST" and str(args.get("url") or "").endswith("/tasks/task-714/defer") and args.get("content_type") == "application/json" and isinstance(body.get("retry_after_minutes"), (int, float)) and body.get("retry_after_minutes") > 0 and bool(str(body.get("reason") or "").strip())
    if kind == "browser":
        return not args or args.get("connection_mode") == "cdp"
    if kind == "batch_shell_read":
        command = str(calls[0].arguments.get("command") or "") if calls else ""
        first_ok = calls and "/workspace/flag.txt" in command and "verified-714" in command
        return bool(first_ok) and (len(calls) == 1 or (len(calls) == 2 and calls[1].arguments.get("file_path") == "/workspace/flag.txt"))
    if kind == "delegate":
        return args.get("target_agent_id") == "payroll-specialist" and "PB-714" in str(args.get("goal") or "")
    if kind == "handover":
        return args.get("target_agent_id") == "browser-specialist" and args.get("preserve_live_execution_context") is True
    return False


REQUIRED = {tool["name"]: set(tool["parameters"].get("required") or []) for tool in TOOLS}


def schema_valid(calls: list[Any]) -> bool:
    if not calls:
        return False
    for call in calls:
        if call.name not in REQUIRED or not REQUIRED[call.name].issubset(call.arguments):
            return False
        if not isinstance(call.arguments, dict):
            return False
    return True


def score(helpers: Any, variant: str, scenario: Scenario, run_index: int, status: int, response: dict[str, Any] | None, total_ms: int, tool_ms: int | None, pricing: dict[str, Any] | None, error: str | None) -> Result:
    response = response or {}
    calls = helpers.parse_tool_calls(response)
    names = tuple(call.name for call in calls)
    text = output_text(response)
    # Ordered batching is optional in the runtime contract ("may issue").
    # For its dedicated probe, a correct first action is a valid iterative
    # decision; if the optional verification call is emitted, it must be the
    # exact second call and pass the argument validator below.
    exact = names == scenario.expected_tools or (
        scenario.validator == "batch_shell_read" and names == scenario.expected_tools[:1]
    )
    args_ok = exact and validate(scenario.validator, calls)
    schema_ok = schema_valid(calls)
    native_ok = status == 200 and bool(calls)
    terminal_positions = [index for index, call in enumerate(calls) if call.name in TERMINAL_TOOLS]
    terminal_ok = not terminal_positions or terminal_positions == [len(calls) - 1]
    legacy_absent = not any(key in call.arguments for call in calls for key in LEGACY_KEYS) and not re.search(r'"decision"\s*:|action_type|capability_name', text)
    passed = native_ok and exact and args_ok and schema_ok and terminal_ok and legacy_absent
    usage = response.get("usage") or {}
    input_tokens = usage.get("input_tokens")
    cached_tokens = (usage.get("input_tokens_details") or {}).get("cached_tokens")
    output_tokens = usage.get("output_tokens")
    reasoning_tokens = (usage.get("output_tokens_details") or {}).get("reasoning_tokens")
    return Result(
        variant, scenario.name, run_index, status, total_ms, tool_ms, input_tokens, cached_tokens,
        output_tokens, reasoning_tokens, helpers.compute_cost(pricing, input_tokens, cached_tokens, output_tokens),
        [asdict(call) for call in calls], text, exact, args_ok, schema_ok, native_ok, terminal_ok,
        bool(legacy_absent), bool(passed), error,
    )


def mean(items: list[Result], field: str) -> float | None:
    values = [getattr(item, field) for item in items if getattr(item, field) is not None]
    return statistics.fmean(values) if values else None


def median(items: list[Result], field: str) -> float | None:
    values = [getattr(item, field) for item in items if getattr(item, field) is not None]
    return statistics.median(values) if values else None


def p90(items: list[Result], field: str) -> float | None:
    values = [getattr(item, field) for item in items if getattr(item, field) is not None]
    if not values:
        return None
    if len(values) == 1:
        return float(values[0])
    return statistics.quantiles(values, n=10, method="inclusive")[8]


def summarize(results: list[Result]) -> dict[str, Any]:
    output: dict[str, Any] = {}
    for variant in VARIANTS:
        subset = [item for item in results if item.variant == variant]
        if not subset:
            continue
        scenarios = {}
        for scenario in SCENARIOS:
            rows = [item for item in subset if item.scenario == scenario.name]
            if rows:
                full_sequence = sum(tuple(call["name"] for call in item.tool_calls) == scenario.expected_tools for item in rows) / len(rows)
                scenarios[scenario.name] = {"calls": len(rows), "pass_rate": sum(item.passed for item in rows) / len(rows), "full_expected_sequence_rate": full_sequence}
        output[variant] = {
            "calls": len(subset),
            "pass_rate": sum(item.passed for item in subset) / len(subset),
            "exact_sequence_rate": sum(item.exact_sequence_pass for item in subset) / len(subset),
            "argument_semantics_rate": sum(item.argument_semantics_pass for item in subset) / len(subset),
            "native_channel_rate": sum(item.native_channel_pass for item in subset) / len(subset),
            "terminal_order_rate": sum(item.terminal_order_pass for item in subset) / len(subset),
            "legacy_envelope_absence_rate": sum(item.legacy_envelope_absent for item in subset) / len(subset),
            "mean_input_tokens": mean(subset, "input_tokens"),
            "mean_cached_tokens": mean(subset, "cached_tokens"),
            "mean_output_tokens": mean(subset, "output_tokens"),
            "mean_reasoning_tokens": mean(subset, "reasoning_tokens"),
            "mean_tool_decision_ms": mean(subset, "tool_decision_ms"),
            "median_tool_decision_ms": median(subset, "tool_decision_ms"),
            "p90_tool_decision_ms": p90(subset, "tool_decision_ms"),
            "mean_total_ms": mean(subset, "total_ms"),
            "median_total_ms": median(subset, "total_ms"),
            "p90_total_ms": p90(subset, "total_ms"),
            "total_cost_usd": sum(item.cost_usd or 0.0 for item in subset),
            "scenarios": scenarios,
        }
    return output


def gate_failures(summary: dict[str, Any]) -> list[str]:
    current = summary.get("current")
    if not current:
        return ["current variant did not run"]
    failures: list[str] = []
    for field in ("pass_rate", "exact_sequence_rate", "argument_semantics_rate", "native_channel_rate", "terminal_order_rate", "legacy_envelope_absence_rate"):
        if current[field] < 1.0:
            failures.append(f"current {field}={current[field]:.1%}; required 100%")
    for name, values in current["scenarios"].items():
        if values["pass_rate"] < 1.0:
            failures.append(f"current scenario {name} pass_rate={values['pass_rate']:.1%}; required 100%")
    previous = summary.get("previous")
    if previous:
        if current["pass_rate"] < previous["pass_rate"]:
            failures.append("current semantic pass rate is lower than previous")
        if current["mean_input_tokens"] is not None and previous["mean_input_tokens"] is not None and current["mean_input_tokens"] >= previous["mean_input_tokens"]:
            failures.append("current mean input tokens are not lower than previous")
    return failures


def render_html(report: dict[str, Any]) -> str:
    cards = []
    for variant, values in report["summary"].items():
        cards.append(f"<section><h2>{variant}</h2><p>Pass <b>{values['pass_rate']:.1%}</b> · Native <b>{values['native_channel_rate']:.1%}</b> · Arguments <b>{values['argument_semantics_rate']:.1%}</b></p><p>Input mean <b>{values['mean_input_tokens'] or 0:.0f}</b> · cached <b>{values['mean_cached_tokens'] or 0:.0f}</b></p><p>Decision ms mean / median / p90<br><b>{values['mean_tool_decision_ms'] or 0:.0f} / {values['median_tool_decision_ms'] or 0:.0f} / {values['p90_tool_decision_ms'] or 0:.0f}</b></p><p>Total ms mean / median / p90<br><b>{values['mean_total_ms'] or 0:.0f} / {values['median_total_ms'] or 0:.0f} / {values['p90_total_ms'] or 0:.0f}</b></p><p>Cost <b>${values['total_cost_usd']:.5f}</b></p></section>")
    rows = []
    for item in report["results"]:
        names = " → ".join(call["name"] for call in item["tool_calls"]) or "—"
        rows.append(f"<tr><td>{item['variant']}</td><td>{item['scenario']}</td><td>{item['run_index']}</td><td class={'pass' if item['passed'] else 'fail'}>{'PASS' if item['passed'] else 'FAIL'}</td><td>{html.escape(names)}</td><td>{item['tool_decision_ms'] or 0}</td><td>{item['input_tokens'] or 0}</td><td>{html.escape(item['error'] or '')}</td></tr>")
    failures = "".join(f"<li>{html.escape(item)}</li>" for item in report["gate_failures"])
    return f"""<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Agentic Native Tool Live A/B</title><style>
body{{font:15px system-ui;background:#0b1020;color:#e8edff;margin:32px}}main{{max-width:1400px;margin:auto}}.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(230px,1fr));gap:16px}}section,table{{background:#141b31;border:1px solid #2c385d;border-radius:12px}}section{{padding:18px}}table{{width:100%;border-collapse:collapse;margin-top:24px}}th,td{{padding:9px;border-bottom:1px solid #293554;text-align:left}}.pass{{color:#5ee6a8;font-weight:700}}.fail{{color:#ff7285;font-weight:700}}
</style></head><body><main><h1>Agentic Native-Tool Contract — Live A/B</h1><p>{html.escape(report['profile']['model'])} · {report['generated_at']} · {report['scenario_count']} scenarios × {report['runs']} repeats</p><h2 class={'pass' if not report['gate_failures'] else 'fail'}>Gate: {'PASS' if not report['gate_failures'] else 'FAIL'}</h2><ul>{failures or '<li>All decisive gates passed.</li>'}</ul><div class=cards>{''.join(cards)}</div><table><thead><tr><th>Variant</th><th>Scenario</th><th>Run</th><th>Result</th><th>Calls</th><th>Decision ms</th><th>Input</th><th>Error</th></tr></thead><tbody>{''.join(rows)}</tbody></table></main></body></html>"""


def write_report(output: Path, report: dict[str, Any]) -> None:
    output.mkdir(parents=True, exist_ok=True)
    (output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    with (output / "calls.jsonl").open("w", encoding="utf-8") as handle:
        for item in report["results"]:
            handle.write(json.dumps(item) + "\n")
    (output / "report.html").write_text(render_html(report), encoding="utf-8")


def self_test(root: Path, helpers: Any) -> None:
    current_prompt = render_prompt(prompt_file(root, "agentic_decision", CURRENT[0]), {})
    previous_prompt = render_prompt(prompt_file(root, "agentic_decision", PREVIOUS[0]), {})
    current_runtime = current_runtime_instruction(root)
    assert current_prompt.count('{"decision":') == 0
    assert previous_prompt.count('{"decision":') == 11
    assert "provider-native tool-calling channel" in current_runtime
    assert "action_type" not in current_runtime and "action_type" in LEGACY_RUNTIME_INSTRUCTION
    fake = {"output": [{"type": "function_call", "name": "read_file", "arguments": json.dumps({"file_path": "/workspace/status.txt"})}], "usage": {"input_tokens": 100, "output_tokens": 10}}
    result = score(helpers, "current", next(s for s in SCENARIOS if s.name == "read_file"), 1, 200, fake, 90, 70, None, None)
    assert result.passed
    with tempfile.TemporaryDirectory(prefix="native-tool-live-eval-") as tmp:
        report = {"profile": {"model": "test"}, "generated_at": "test", "scenario_count": 1, "runs": 1, "summary": summarize([result]), "gate_failures": [], "results": [asdict(result)]}
        write_report(Path(tmp), report)
        assert (Path(tmp) / "report.html").is_file()
    print("agentic native-tool live evaluator self-test passed")


def parse_args(root: Path, helpers: Any) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=helpers.default_config_path(root))
    parser.add_argument("--profile")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--variant", choices=("both", "current", "previous"), default="both")
    parser.add_argument("--scenario", action="append", choices=[item.name for item in SCENARIOS])
    parser.add_argument("--max-output-tokens", type=int, default=DEFAULT_MAX_OUTPUT_TOKENS)
    parser.add_argument("--timeout-secs", type=int)
    parser.add_argument("--pricing-file", type=Path)
    parser.add_argument("--env-file", type=Path, action="append")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--render-report", type=Path, help="Recompute summaries and HTML from an existing report.json without provider calls")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--no-gate", action="store_true")
    return parser.parse_args()


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    helpers = load_helpers(root)
    args = parse_args(root, helpers)
    if args.self_test:
        self_test(root, helpers)
        return 0
    if args.render_report:
        report_path = args.render_report.expanduser().resolve()
        report = json.loads(report_path.read_text(encoding="utf-8"))
        results = [Result(**item) for item in report["results"]]
        report["summary"] = summarize(results)
        report["gate_failures"] = gate_failures(report["summary"])
        write_report(report_path.parent, report)
        print(f"Re-rendered JSON report: {report_path}")
        print(f"Re-rendered HTML report: {report_path.parent / 'report.html'}")
        return 0 if not report["gate_failures"] or args.no_gate else 1
    if args.runs < 1 or args.workers < 1:
        print("--runs and --workers must be positive", file=sys.stderr)
        return 2
    try:
        profile = helpers.load_profile(args.config.expanduser(), args.profile)
    except Exception as error:
        print(f"profile configuration error: {error}", file=sys.stderr)
        return 2
    if profile.provider.lower() != "openai":
        print(f"live eval requires an OpenAI Responses profile; got {profile.provider}", file=sys.stderr)
        return 2
    scenarios = [item for item in SCENARIOS if not args.scenario or item.name in args.scenario]
    variants = ["current", "previous"] if args.variant == "both" else [args.variant]
    effective_max = min(profile.configured_max_output_tokens, args.max_output_tokens)
    jobs: list[tuple[int, Scenario, str]] = []
    for run_index in range(1, args.runs + 1):
        for scenario_index, scenario in enumerate(scenarios):
            ordered = list(variants)
            if len(ordered) == 2 and (run_index + scenario_index) % 2 == 0:
                ordered.reverse()
            jobs.extend((run_index, scenario, variant) for variant in ordered)
    projected = len(jobs)
    print(f"Live native-tool eval: profile={profile.name} model={profile.model} scenarios={len(scenarios)} runs={args.runs} variants={','.join(variants)} calls={projected} workers={args.workers}")
    if args.dry_run:
        sample_current = build_payload(root, profile, scenarios[0], variants[0], effective_max)
        sample_previous = build_payload(root, profile, scenarios[0], variants[-1], effective_max)
        print(json.dumps({"config": str(args.config), "profile": asdict(profile), "projected_calls": projected, "current_payload_bytes": len(json.dumps(sample_current).encode()), "comparison_payload_bytes": len(json.dumps(sample_previous).encode()), "scenarios": [asdict(item) for item in scenarios]}, indent=2))
        return 0
    for env_file in args.env_file or [Path.home() / "MagicianNotes/.env.development", Path.home() / "MagicianNotes/.env", root / ".env.development", root / ".env"]:
        if env_file.is_file():
            helpers.load_dotenv(env_file)
    api_key = os.environ.get(profile.api_key_env)
    if not api_key:
        print(f"{profile.api_key_env} is not set", file=sys.stderr)
        return 2
    pricing_path = args.pricing_file or Path.home() / "MagicianNotes/llm_pricing.json"
    pricing = helpers.select_pricing_row(pricing_path, profile.provider, profile.model) if pricing_path.is_file() else None
    endpoint = helpers.responses_url(profile)
    timeout = args.timeout_secs or profile.timeout_secs

    def execute(job: tuple[int, Scenario, str]) -> Result:
        run_index, scenario, variant = job
        payload = build_payload(root, profile, scenario, variant, effective_max)
        status, response, total_ms, _first_ms, tool_ms, error = helpers.run_live_request(api_key, endpoint, payload, timeout)
        result = score(helpers, variant, scenario, run_index, status, response, total_ms, tool_ms, pricing, error)
        names = " -> ".join(item["name"] for item in result.tool_calls) or "<none>"
        print(f"  {variant}/{scenario.name}/run-{run_index}: HTTP {status} tools={names} decision={tool_ms}ms {'PASS' if result.passed else 'FAIL'}", flush=True)
        return result

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        results = list(pool.map(execute, jobs))
    summary = summarize(results)
    failures = gate_failures(summary)
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output = args.output_dir or root / "coverage/evals/agentic-native-tool-contract" / f"live-{stamp}"
    report = {"profile": asdict(profile), "generated_at": stamp, "variants": VARIANTS, "scenario_count": len(scenarios), "runs": args.runs, "workers": args.workers, "summary": summary, "gate_failures": failures, "results": [asdict(item) for item in results]}
    write_report(output, report)
    print(f"JSON report: {output / 'report.json'}")
    print(f"HTML report: {output / 'report.html'}")
    if failures:
        for failure in failures:
            print(f"GATE FAIL: {failure}", file=sys.stderr)
        return 0 if args.no_gate else 1
    print("Agentic native-tool live gate: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

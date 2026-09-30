#!/usr/bin/env python3
"""Run a bounded live A/B eval for the agentic terminal-tool contract."""

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


DEFAULT_MAX_OUTPUT_TOKENS = 2048
RETIRED_TERMINALS = ("goal_reached", "cannot_proceed")


def load_helpers(repo_root: Path) -> Any:
    path = repo_root / "scripts/eval-agentic-decision-rationale-live.py"
    spec = importlib.util.spec_from_file_location("magician_terminal_eval_helpers", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load live-eval helpers from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


@dataclass(frozen=True)
class Scenario:
    name: str
    goal: str
    criteria: str
    state: str
    expected_tool: str
    expected_shape: str


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
    expected_tool_pass: bool
    terminal_shape_pass: bool
    retired_terminal_absent: bool
    error: str | None


SCENARIOS = (
    Scenario(
        name="already_complete",
        goal="Determine the deployed release identifier.",
        criteria="Return the exact release identifier visible in the current state.",
        state="Deployment status is healthy. Release identifier: magician-2026.07.14. No action is needed.",
        expected_tool="yield",
        expected_shape="complete",
    ),
    Scenario(
        name="partial_with_blocker",
        goal="Verify both production regions and report their release identifiers.",
        criteria="Report a verified identifier for us-east and eu-west.",
        state=(
            "us-east is verified at magician-2026.07.14. eu-west could not be inspected because "
            "the regional API returned HTTP 403 and no alternate credential is available."
        ),
        expected_tool="yield",
        expected_shape="partial",
    ),
    Scenario(
        name="fully_blocked",
        goal="Read the protected production release identifier.",
        criteria="Return the exact identifier from the protected production API.",
        state=(
            "Nothing has been completed. The only production API returns HTTP 403, the required "
            "credential is unavailable, and retrying cannot change that permission state."
        ),
        expected_tool="yield",
        expected_shape="blocked",
    ),
    Scenario(
        name="needs_user_input",
        goal="Deploy the approved build to production.",
        criteria="Deploy to the production account selected by the user.",
        state=(
            "Two production accounts are available, prod-blue and prod-green. The request does "
            "not identify which account to use, and choosing one without the user would be unsafe."
        ),
        expected_tool="need_user_input",
        expected_shape="question",
    ),
    Scenario(
        name="work_remains",
        goal="Read the release identifier from /workspace/status.txt.",
        criteria="Return the identifier stored in that file.",
        state="The file has not been read. The shell tool is available and the path is known.",
        expected_tool="shell",
        expected_shape="shell",
    ),
)


TOOLS: tuple[dict[str, Any], ...] = (
    {
        "type": "function",
        "name": "yield",
        "description": "Return the terminal outcome of the current goal.",
        "parameters": {
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "completed": {"type": "array", "items": {"type": "string"}},
                "open": {"type": "array", "items": {"type": "string"}},
                "blockers": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": {
                                "type": "string",
                                "enum": ["auth", "permission", "data_missing", "transient", "external", "other"],
                            },
                            "description": {"type": "string"},
                        },
                        "required": ["kind", "description"],
                        "additionalProperties": False,
                    },
                },
                "artifacts": {"type": "array", "items": {"type": "object"}},
                "next_step_hint": {"type": "string"},
            },
            "required": ["summary"],
            "additionalProperties": False,
        },
    },
    {
        "type": "function",
        "name": "need_user_input",
        "description": "Ask the user for information or a choice required to continue.",
        "parameters": {
            "type": "object",
            "properties": {
                "question": {"type": "string"},
                "input_type": {"type": "string", "enum": ["text", "confirmation", "choice"]},
            },
            "required": ["question", "input_type"],
            "additionalProperties": False,
        },
    },
    {
        "type": "function",
        "name": "shell",
        "description": "Run one bounded shell command to inspect the workspace.",
        "parameters": {
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
            "additionalProperties": False,
        },
    },
)


VARIANTS = {
    "current": ("1.3.7", "1.0.6"),
    "previous": ("1.3.5", "1.0.4"),
}


def prompt_file(repo_root: Path, name: str, version: str) -> Path:
    return repo_root / "data/magician_v2/prompts" / f"{name}_v{version}.json"


def render_prompt(path: Path, values: dict[str, str]) -> str:
    raw = json.loads(path.read_text(encoding="utf-8"))
    rendered = "\n".join(raw["content"])
    variables = raw.get("variables") or []
    if variables and isinstance(variables[0], str):
        names = variables
        defaults: dict[str, str] = {}
    else:
        names = [item["name"] for item in variables]
        defaults = {
            item["name"]: str(item.get("default_value") or "")
            for item in variables
            if isinstance(item, dict)
        }
    for name in names:
        rendered = rendered.replace("{" + name + "}", values.get(name, defaults.get(name, "")))
    leftovers = [name for name in names if "{" + name + "}" in rendered]
    if leftovers:
        raise ValueError(f"unrendered variables in {path}: {leftovers}")
    return rendered


def build_payload(repo_root: Path, helpers: Any, profile: Any, scenario: Scenario, variant: str, max_tokens: int) -> dict[str, Any]:
    decision_version, system_version = VARIANTS[variant]
    capabilities = "Available native tools: `yield`, `need_user_input`, and `shell`. Tool schemas are authoritative."
    system = render_prompt(
        prompt_file(repo_root, "agentic_decision_system", system_version),
        {"identity_section": "", "capabilities_section": capabilities},
    )
    user = render_prompt(
        prompt_file(repo_root, "agentic_decision", decision_version),
        {
            "goal": scenario.goal,
            "success_criteria": scenario.criteria,
            "state_type": "evaluation fixture",
            "state_description": scenario.state,
            "history_summary": "No actions executed yet.",
        },
    )
    payload: dict[str, Any] = {
        "model": profile.model,
        "instructions": system,
        "input": [{"role": "user", "content": [{"type": "input_text", "text": user}]}],
        "tools": list(TOOLS),
        "tool_choice": "required",
        "max_output_tokens": max_tokens,
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


def shape_pass(shape: str, arguments: dict[str, Any]) -> bool:
    completed = arguments.get("completed") or []
    open_items = arguments.get("open") or []
    blockers = arguments.get("blockers") or []
    if shape == "complete":
        return bool(completed) and not open_items and not blockers
    if shape == "partial":
        return bool(completed) and bool(open_items) and bool(blockers)
    if shape == "blocked":
        return not completed and bool(open_items) and bool(blockers)
    if shape == "question":
        return bool(str(arguments.get("question") or "").strip())
    if shape == "shell":
        command = str(arguments.get("command") or "")
        return "/workspace/status.txt" in command and any(token in command for token in ("cat", "sed", "awk", "head", "tail"))
    return False


def score(helpers: Any, variant: str, scenario: Scenario, run_index: int, status: int, response: dict[str, Any] | None, total_ms: int, tool_ms: int | None, pricing: dict[str, Any] | None, error: str | None) -> Result:
    response = response or {}
    calls = helpers.parse_tool_calls(response)
    serialized = [asdict(call) for call in calls]
    expected = len(calls) == 1 and calls[0].name == scenario.expected_tool
    shape_ok = expected and shape_pass(scenario.expected_shape, calls[0].arguments)
    retired_absent = all(call.name not in RETIRED_TERMINALS for call in calls)
    usage = response.get("usage") or {}
    input_tokens = usage.get("input_tokens")
    cached_tokens = (usage.get("input_tokens_details") or {}).get("cached_tokens")
    output_tokens = usage.get("output_tokens")
    reasoning_tokens = (usage.get("output_tokens_details") or {}).get("reasoning_tokens")
    return Result(
        variant=variant,
        scenario=scenario.name,
        run_index=run_index,
        status_code=status,
        total_ms=total_ms,
        tool_decision_ms=tool_ms,
        input_tokens=input_tokens,
        cached_tokens=cached_tokens,
        output_tokens=output_tokens,
        reasoning_tokens=reasoning_tokens,
        cost_usd=helpers.compute_cost(pricing, input_tokens, cached_tokens, output_tokens),
        tool_calls=serialized,
        expected_tool_pass=expected,
        terminal_shape_pass=shape_ok,
        retired_terminal_absent=retired_absent,
        error=error,
    )


def mean(results: list[Result], field: str) -> float | None:
    values = [getattr(item, field) for item in results if getattr(item, field) is not None]
    return statistics.fmean(values) if values else None


def summarize(results: list[Result]) -> dict[str, Any]:
    output: dict[str, Any] = {}
    for variant in VARIANTS:
        subset = [item for item in results if item.variant == variant]
        if not subset:
            continue
        output[variant] = {
            "calls": len(subset),
            "http_success_rate": sum(item.status_code == 200 for item in subset) / len(subset),
            "tool_selection_rate": sum(item.expected_tool_pass for item in subset) / len(subset),
            "terminal_shape_rate": sum(item.terminal_shape_pass for item in subset) / len(subset),
            "retired_terminal_absence_rate": sum(item.retired_terminal_absent for item in subset) / len(subset),
            "mean_tool_decision_ms": mean(subset, "tool_decision_ms"),
            "mean_total_ms": mean(subset, "total_ms"),
            "mean_input_tokens": mean(subset, "input_tokens"),
            "total_cost_usd": sum(item.cost_usd or 0.0 for item in subset),
        }
    return output


def gate_failures(summary: dict[str, Any]) -> list[str]:
    current = summary.get("current")
    if current is None:
        return ["current variant did not run"]
    failures = []
    for field in ("http_success_rate", "tool_selection_rate", "terminal_shape_rate", "retired_terminal_absence_rate"):
        if current[field] < 1.0:
            failures.append(f"current {field}={current[field]:.1%}; required 100%")
    previous = summary.get("previous")
    if previous and current["tool_selection_rate"] + 0.2 < previous["tool_selection_rate"]:
        failures.append("current tool selection regressed by more than 20 percentage points versus previous")
    return failures


def render_html(report: dict[str, Any]) -> str:
    cards = []
    for variant, values in report["summary"].items():
        cards.append(
            f"<section><h2>{html.escape(variant)}</h2>"
            f"<p>Tool selection <b>{values['tool_selection_rate']:.0%}</b></p>"
            f"<p>Terminal shape <b>{values['terminal_shape_rate']:.0%}</b></p>"
            f"<p>Retired names absent <b>{values['retired_terminal_absence_rate']:.0%}</b></p>"
            f"<p>Mean decision <b>{values['mean_tool_decision_ms'] or 0:.0f} ms</b></p>"
            f"<p>Cost <b>${values['total_cost_usd']:.5f}</b></p></section>"
        )
    rows = []
    for item in report["results"]:
        passed = item["status_code"] == 200 and item["expected_tool_pass"] and item["terminal_shape_pass"] and item["retired_terminal_absent"]
        names = " → ".join(call["name"] for call in item["tool_calls"]) or "—"
        rows.append(
            f"<tr><td>{html.escape(item['variant'])}</td><td>{html.escape(item['scenario'])}</td>"
            f"<td class={'pass' if passed else 'fail'}>{'PASS' if passed else 'FAIL'}</td>"
            f"<td>{html.escape(names)}</td><td>{item['tool_decision_ms'] or 0}</td>"
            f"<td>{item['input_tokens'] or 0}</td><td>${item['cost_usd'] or 0:.5f}</td></tr>"
        )
    failures = "".join(f"<li>{html.escape(item)}</li>" for item in report["gate_failures"])
    return f"""<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Agentic Terminal Contract Eval</title><style>
body{{font:15px system-ui;background:#0b1020;color:#e8edff;margin:32px}}main{{max-width:1200px;margin:auto}}.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(230px,1fr));gap:16px}}section,table{{background:#141b31;border:1px solid #2c385d;border-radius:12px}}section{{padding:18px}}table{{width:100%;border-collapse:collapse;margin-top:24px}}th,td{{padding:10px;border-bottom:1px solid #293554;text-align:left}}.pass{{color:#5ee6a8;font-weight:700}}.fail{{color:#ff7285;font-weight:700}}
</style></head><body><main><h1>Agentic Terminal Contract — Live A/B</h1><p>{html.escape(report['profile']['model'])} · {html.escape(report['generated_at'])}</p><h2 class={'pass' if not failures else 'fail'}>Gate: {'PASS' if not failures else 'FAIL'}</h2><ul>{failures or '<li>All current-contract gates passed.</li>'}</ul><div class=cards>{''.join(cards)}</div><table><thead><tr><th>Variant</th><th>Scenario</th><th>Result</th><th>Tool</th><th>Decision ms</th><th>Input</th><th>Cost</th></tr></thead><tbody>{''.join(rows)}</tbody></table></main></body></html>"""


def write_report(output_dir: Path, report: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    with (output_dir / "calls.jsonl").open("w", encoding="utf-8") as handle:
        for item in report["results"]:
            handle.write(json.dumps(item) + "\n")
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")


def run_self_test(repo_root: Path, helpers: Any) -> None:
    current_decision = render_prompt(prompt_file(repo_root, "agentic_decision", "1.3.7"), {})
    current_system = render_prompt(prompt_file(repo_root, "agentic_decision_system", "1.0.6"), {})
    previous_decision = render_prompt(prompt_file(repo_root, "agentic_decision", "1.3.5"), {})
    assert all(name not in current_decision and name not in current_system for name in RETIRED_TERMINALS)
    assert all(name in previous_decision for name in RETIRED_TERMINALS)
    assert {tool["name"] for tool in TOOLS} == {"yield", "need_user_input", "shell"}
    fake = {
        "output": [{"type": "function_call", "name": "yield", "arguments": json.dumps({"summary": "done", "completed": ["release read"]})}],
        "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 20}, "output_tokens": 20},
    }
    result = score(helpers, "current", SCENARIOS[0], 1, 200, fake, 100, 80, None, None)
    assert result.expected_tool_pass and result.terminal_shape_pass and result.retired_terminal_absent
    with tempfile.TemporaryDirectory(prefix="terminal-contract-eval-") as tmp:
        report = {"profile": {"model": "test"}, "generated_at": "test", "summary": summarize([result]), "gate_failures": [], "results": [asdict(result)]}
        write_report(Path(tmp), report)
        assert (Path(tmp) / "report.html").is_file()
    print("agentic terminal-contract evaluator self-test passed")


def parse_args(repo_root: Path, helpers: Any) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=helpers.default_config_path(repo_root))
    parser.add_argument("--profile", help="Override operation_mapping.agentic_decision")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--variant", choices=("both", "current", "previous"), default="both")
    parser.add_argument("--scenario", action="append", choices=[item.name for item in SCENARIOS])
    parser.add_argument("--max-output-tokens", type=int, default=DEFAULT_MAX_OUTPUT_TOKENS)
    parser.add_argument("--timeout-secs", type=int)
    parser.add_argument("--pricing-file", type=Path)
    parser.add_argument("--env-file", type=Path, action="append")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--no-gate", action="store_true")
    return parser.parse_args()


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    helpers = load_helpers(repo_root)
    args = parse_args(repo_root, helpers)
    if args.self_test:
        run_self_test(repo_root, helpers)
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
        print(f"terminal live eval requires an OpenAI Responses profile; got {profile.provider}", file=sys.stderr)
        return 2
    scenarios = [item for item in SCENARIOS if not args.scenario or item.name in args.scenario]
    variants = ["current", "previous"] if args.variant == "both" else [args.variant]
    effective_max = min(profile.configured_max_output_tokens, args.max_output_tokens)
    projected = len(scenarios) * len(variants) * args.runs
    print(f"Live terminal eval: profile={profile.name} model={profile.model} scenarios={len(scenarios)} variants={','.join(variants)} calls={projected}")
    if args.dry_run:
        sample = build_payload(repo_root, helpers, profile, scenarios[0], variants[0], effective_max)
        print(json.dumps({"config": str(args.config), "profile": asdict(profile), "projected_calls": projected, "sample_payload_bytes": len(json.dumps(sample).encode()), "scenarios": [asdict(item) for item in scenarios]}, indent=2))
        return 0
    for env_file in args.env_file or [Path.home() / "MagicianNotes/.env.development", Path.home() / "MagicianNotes/.env", repo_root / ".env.development", repo_root / ".env"]:
        if env_file.is_file():
            helpers.load_dotenv(env_file)
    api_key = os.environ.get(profile.api_key_env)
    if not api_key:
        print(f"{profile.api_key_env} is not set", file=sys.stderr)
        return 2
    pricing_path = args.pricing_file or (Path.home() / "MagicianNotes/llm_pricing.json")
    pricing = helpers.select_pricing_row(pricing_path, profile.provider, profile.model) if pricing_path.is_file() else None
    endpoint = helpers.responses_url(profile)
    timeout = args.timeout_secs or profile.timeout_secs
    results: list[Result] = []
    for run_index in range(1, args.runs + 1):
        for scenario_index, scenario in enumerate(scenarios):
            ordered = list(variants)
            if len(ordered) == 2 and (run_index + scenario_index) % 2 == 0:
                ordered.reverse()
            for variant in ordered:
                print(f"  [{len(results) + 1}/{projected}] {variant}/{scenario.name} ...", flush=True)
                payload = build_payload(repo_root, helpers, profile, scenario, variant, effective_max)
                status, response, total_ms, _first_ms, tool_ms, error = helpers.run_live_request(api_key, endpoint, payload, timeout)
                result = score(helpers, variant, scenario, run_index, status, response, total_ms, tool_ms, pricing, error)
                results.append(result)
                names = " -> ".join(item["name"] for item in result.tool_calls) or "<none>"
                print(f"      HTTP {status} tools={names} decision={tool_ms}ms total={total_ms}ms result={'pass' if result.expected_tool_pass and result.terminal_shape_pass else 'FAIL'}")
    summary = summarize(results)
    failures = gate_failures(summary)
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output_dir = args.output_dir or repo_root / "coverage/evals/agentic-terminal-contract" / timestamp
    report = {"profile": asdict(profile), "generated_at": timestamp, "variants": VARIANTS, "summary": summary, "gate_failures": failures, "results": [asdict(item) for item in results]}
    write_report(output_dir, report)
    print(f"JSON report: {output_dir / 'report.json'}")
    print(f"HTML report: {output_dir / 'report.html'}")
    if failures:
        for failure in failures:
            print(f"GATE FAIL: {failure}", file=sys.stderr)
        return 0 if args.no_gate else 1
    print("Terminal-contract gate: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

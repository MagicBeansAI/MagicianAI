#!/usr/bin/env python3
"""Run a bounded real-model A/B eval for compact decision metadata."""

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
METADATA_FIELDS = (
    "request_hover_discovery",
    "request_vision",
    "vision_reason",
    "step_completed",
    "step_failed",
    "needs_plan_revision",
)


def load_live_helpers(repo_root: Path) -> Any:
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
    prompt: str
    expected_tool: str
    expected_argument: tuple[str, str]
    expected_metadata: dict[str, Any]


@dataclass
class EvalResult:
    variant: str
    scenario: str
    run_index: int
    status_code: int
    response_status: str | None
    total_ms: int
    first_output_ms: int | None
    tool_decision_ms: int | None
    input_tokens: int | None
    cached_tokens: int | None
    output_tokens: int | None
    reasoning_tokens: int | None
    cost_usd: float | None
    tool_calls: list[dict[str, Any]]
    observed_metadata: dict[str, Any]
    tool_selection_pass: bool
    representation_contract_pass: bool
    signal_parity_pass: bool
    sparse_omission_pass: bool
    error: str | None


SCENARIOS = (
    Scenario(
        name="ordinary_no_metadata",
        prompt=(
            "Goal: determine the current working directory. No plan step changed, text is "
            "sufficient, and no execution metadata signal applies. Call shell with pwd now."
        ),
        expected_tool="shell",
        expected_argument=("command", "pwd"),
        expected_metadata={},
    ),
    Scenario(
        name="completed_step",
        prompt=(
            "The observable state proves plan step step-3 is complete. Continue by calling "
            "search_logs for checkout-service. Report step_completed exactly as step-3 on "
            "that selected call."
        ),
        expected_tool="search_logs",
        expected_argument=("service", "checkout-service"),
        expected_metadata={"step_completed": "step-3"},
    ),
    Scenario(
        name="failed_step_revision",
        prompt=(
            "Plan step step-4 is now blocked because the required configuration is absent, "
            "and the plan must be revised. Call read_file for /workspace/fallback.yaml. "
            "Report step_failed exactly as step-4 and needs_plan_revision true."
        ),
        expected_tool="read_file",
        expected_argument=("path", "/workspace/fallback.yaml"),
        expected_metadata={"step_failed": "step-4", "needs_plan_revision": True},
    ),
    Scenario(
        name="vision_hover_escalation",
        prompt=(
            "The text observation is insufficient to identify the obscured control. Call "
            "inspect_screen with region main. Request both hover discovery and vision, and "
            "include a non-empty vision reason on that selected call."
        ),
        expected_tool="inspect_screen",
        expected_argument=("region", "main"),
        expected_metadata={
            "request_hover_discovery": True,
            "request_vision": True,
            "vision_reason": "<nonempty>",
        },
    ),
)


BASE_TOOLS: tuple[dict[str, Any], ...] = (
    {
        "name": "shell",
        "description": "Run one bounded shell command.",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    },
    {
        "name": "search_logs",
        "description": "Search recent logs for one service.",
        "properties": {
            "service": {"type": "string"},
            "query": {"type": "string"},
        },
        "required": ["service"],
    },
    {
        "name": "read_file",
        "description": "Read one workspace file.",
        "properties": {"path": {"type": "string"}},
        "required": ["path"],
    },
    {
        "name": "inspect_screen",
        "description": "Inspect one named screen region.",
        "properties": {"region": {"type": "string"}},
        "required": ["region"],
    },
    {
        "name": "write_file",
        "description": "Write one workspace file.",
        "properties": {
            "path": {"type": "string"},
            "content": {"type": "string"},
        },
        "required": ["path", "content"],
    },
    {
        "name": "restart_service",
        "description": "Restart one service after diagnosis.",
        "properties": {"service": {"type": "string"}},
        "required": ["service"],
    },
    {
        "name": "yield",
        "description": "Report a terminal outcome.",
        "properties": {"summary": {"type": "string"}},
        "required": ["summary"],
    },
)


LEGACY_SCHEMAS: dict[str, dict[str, Any]] = {
    "request_hover_discovery": {
        "type": "boolean",
        "description": "Request hover probing for next observation",
    },
    "request_vision": {
        "type": "boolean",
        "description": "TEXT-FIRST: request vision escalation when text insufficient",
    },
    "vision_reason": {
        "type": "string",
        "description": "Reason for vision escalation request",
    },
    "step_completed": {
        "type": "string",
        "description": "Step ID completed by this action",
    },
    "step_failed": {
        "type": "string",
        "description": "Step ID that failed/blocked",
    },
    "needs_plan_revision": {
        "type": "boolean",
        "description": "Set true when plan needs revision",
    },
}


BASE_INSTRUCTIONS = """\
You are an agentic executor. Select exactly one tool call that makes the requested
progress. Tool definitions are authoritative. Do not answer in natural language.
Only emit execution metadata explicitly required by the scenario. Never emit false,
null, or empty metadata values.
"""


def variant_instruction(variant: str) -> str:
    if variant == "current":
        return """
Current contract: sparse execution signals belong in one optional
decision_metadata object on the selected tool call. Allowed fields are
request_hover_discovery (boolean), request_vision (boolean), vision_reason
(non-empty string with request_vision), step_completed (step ID), step_failed
(step ID), and needs_plan_revision (boolean). Omit decision_metadata entirely
when no signal applies. Never emit those fields flat.
"""
    if variant == "legacy":
        return """
Legacy contract: emit requested sparse execution signals as flat fields on the
selected tool call. Omit every flat signal when none applies. Never emit a
decision_metadata object.
"""
    raise ValueError(f"unknown variant: {variant}")


def build_tools(variant: str) -> list[dict[str, Any]]:
    tools: list[dict[str, Any]] = []
    for base in BASE_TOOLS:
        properties = dict(base["properties"])
        properties["thinking"] = {"type": "string"}
        if variant == "current":
            properties["decision_metadata"] = {"type": "object"}
        elif variant == "legacy":
            properties.update(LEGACY_SCHEMAS)
        else:
            raise ValueError(f"unknown variant: {variant}")
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


def build_payload(profile: Any, scenario: Scenario, variant: str, limit: int) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "model": profile.model,
        "instructions": BASE_INSTRUCTIONS + variant_instruction(variant),
        "input": [{"role": "user", "content": [{"type": "input_text", "text": scenario.prompt}]}],
        "tools": build_tools(variant),
        "tool_choice": "required",
        "max_output_tokens": limit,
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


def schema_metrics() -> dict[str, Any]:
    current = len(json.dumps(build_tools("current"), separators=(",", ":")).encode())
    legacy = len(json.dumps(build_tools("legacy"), separators=(",", ":")).encode())
    return {
        "current_catalog_bytes": current,
        "legacy_catalog_bytes": legacy,
        "catalog_savings_bytes": legacy - current,
        "catalog_reduction_pct": 100 * (legacy - current) / legacy,
    }


def extract_metadata(variant: str, arguments: dict[str, Any]) -> tuple[dict[str, Any], bool]:
    if variant == "current":
        raw = arguments.get("decision_metadata")
        representation_ok = raw is None or isinstance(raw, dict)
        representation_ok = representation_ok and not any(key in arguments for key in METADATA_FIELDS)
        return (dict(raw) if isinstance(raw, dict) else {}), representation_ok
    representation_ok = "decision_metadata" not in arguments
    return ({key: arguments[key] for key in METADATA_FIELDS if key in arguments}, representation_ok)


def metadata_contract_pass(metadata: dict[str, Any]) -> bool:
    if any(key not in METADATA_FIELDS for key in metadata):
        return False
    for key, value in metadata.items():
        if value is None or value is False or value == "":
            return False
        if key in {"request_hover_discovery", "request_vision", "needs_plan_revision"}:
            if not isinstance(value, bool):
                return False
        elif not isinstance(value, str):
            return False
    return True


def signal_matches(expected: dict[str, Any], observed: dict[str, Any]) -> bool:
    if set(expected) != set(observed):
        return False
    for key, value in expected.items():
        if value == "<nonempty>":
            if not isinstance(observed.get(key), str) or not observed[key].strip():
                return False
        elif observed.get(key) != value:
            return False
    return True


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
    arguments = calls[0].arguments if len(calls) == 1 else {}
    observed, representation_ok = extract_metadata(variant, arguments)
    arg_name, arg_value = scenario.expected_argument
    selection = (
        len(calls) == 1
        and calls[0].name == scenario.expected_tool
        and str(arguments.get(arg_name, "")).strip() == arg_value
    )
    usage = response.get("usage") or {}
    input_tokens = usage.get("input_tokens")
    cached_tokens = (usage.get("input_tokens_details") or {}).get("cached_tokens")
    output_tokens = usage.get("output_tokens")
    reasoning_tokens = (usage.get("output_tokens_details") or {}).get("reasoning_tokens")
    return EvalResult(
        variant=variant,
        scenario=scenario.name,
        run_index=run_index,
        status_code=status_code,
        response_status=response.get("status"),
        total_ms=total_ms,
        first_output_ms=first_output_ms,
        tool_decision_ms=tool_decision_ms,
        input_tokens=input_tokens,
        cached_tokens=cached_tokens,
        output_tokens=output_tokens,
        reasoning_tokens=reasoning_tokens,
        cost_usd=helpers.compute_cost(pricing_row, input_tokens, cached_tokens, output_tokens),
        tool_calls=[asdict(call) for call in calls],
        observed_metadata=observed,
        tool_selection_pass=selection,
        representation_contract_pass=representation_ok and metadata_contract_pass(observed),
        signal_parity_pass=signal_matches(scenario.expected_metadata, observed),
        sparse_omission_pass=bool(scenario.expected_metadata) or not observed,
        error=error,
    )


def mean(results: list[EvalResult], field: str) -> float | None:
    values = [getattr(item, field) for item in results if getattr(item, field) is not None]
    return statistics.fmean(values) if values else None


def rate(results: list[EvalResult], field: str) -> float:
    return sum(bool(getattr(item, field)) for item in results) / len(results) if results else 0.0


def summarize(results: list[EvalResult]) -> dict[str, Any]:
    variants: dict[str, Any] = {}
    for variant in sorted({item.variant for item in results}):
        subset = [item for item in results if item.variant == variant]
        variants[variant] = {
            "calls": len(subset),
            "http_success_rate": sum(item.status_code == 200 for item in subset) / len(subset),
            "tool_selection_rate": rate(subset, "tool_selection_pass"),
            "representation_contract_rate": rate(subset, "representation_contract_pass"),
            "signal_parity_rate": rate(subset, "signal_parity_pass"),
            "sparse_omission_rate": rate(subset, "sparse_omission_pass"),
            "mean_input_tokens": mean(subset, "input_tokens"),
            "mean_tool_decision_ms": mean(subset, "tool_decision_ms"),
            "mean_total_ms": mean(subset, "total_ms"),
            "total_cost_usd": sum(item.cost_usd or 0.0 for item in subset),
        }
    return {"variants": variants}


def evaluate_gates(summary: dict[str, Any]) -> list[str]:
    current = summary["variants"].get("current")
    if current is None:
        return []
    failures: list[str] = []
    for field in (
        "http_success_rate",
        "tool_selection_rate",
        "representation_contract_rate",
        "signal_parity_rate",
        "sparse_omission_rate",
    ):
        if float(current.get(field, 0.0)) < 1.0:
            failures.append(f"current {field}={current.get(field, 0.0):.1%} below 100%")
    legacy = summary["variants"].get("legacy")
    if legacy is not None and float(current["signal_parity_rate"]) < float(legacy["signal_parity_rate"]):
        failures.append("current signal parity is below legacy")
    return failures


def render_html(report: dict[str, Any]) -> str:
    cards = []
    for variant, metrics in report["summary"]["variants"].items():
        cards.append(
            f"<section><h2>{html.escape(variant)}</h2>"
            f"<p>Signal parity <b>{metrics['signal_parity_rate']:.0%}</b></p>"
            f"<p>Representation contract <b>{metrics['representation_contract_rate']:.0%}</b></p>"
            f"<p>Tool selection <b>{metrics['tool_selection_rate']:.0%}</b></p>"
            f"<p>Mean input tokens <b>{metrics['mean_input_tokens'] or 0:,.0f}</b></p>"
            f"<p>Mean decision <b>{metrics['mean_tool_decision_ms'] or 0:,.0f} ms</b></p></section>"
        )
    rows = []
    for item in report["results"]:
        passed = all(
            (item["status_code"] == 200, item["tool_selection_pass"],
             item["representation_contract_pass"], item["signal_parity_pass"])
        )
        rows.append(
            "<tr>"
            f"<td>{html.escape(item['variant'])}</td><td>{html.escape(item['scenario'])}</td>"
            f"<td class={'pass' if passed else 'fail'}>{'PASS' if passed else 'FAIL'}</td>"
            f"<td><code>{html.escape(json.dumps(item['observed_metadata']))}</code></td>"
            f"<td>{item['input_tokens'] or 0}</td><td>{item['tool_decision_ms'] or 0}</td>"
            "</tr>"
        )
    gate = "PASS" if not report["gate_failures"] else "FAIL"
    metrics = report["schema_metrics"]
    return f"""<!doctype html><html lang=en><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1"><title>Decision Metadata Live Eval</title>
<style>body{{font-family:system-ui;background:#0b1020;color:#e8edff;margin:32px}}main{{max-width:1300px;margin:auto}}
.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(250px,1fr));gap:16px}}section{{background:#151d34;padding:18px;border-radius:12px}}
table{{width:100%;border-collapse:collapse;margin-top:24px}}th,td{{padding:10px;border-bottom:1px solid #34405f;text-align:left}}.pass{{color:#63e6aa}}.fail{{color:#ff7187}}code{{white-space:pre-wrap}}</style></head>
<body><main><h1>Decision Metadata Compaction — Live LLM A/B</h1>
<p>{html.escape(report['profile']['name'])} · {html.escape(report['profile']['model'])} · {html.escape(report['generated_at'])}</p>
<h2>Gate: <span class={'pass' if gate == 'PASS' else 'fail'}>{gate}</span></h2>
<p>Catalog: {metrics['current_catalog_bytes']:,} vs {metrics['legacy_catalog_bytes']:,} bytes; saved {metrics['catalog_savings_bytes']:,} bytes ({metrics['catalog_reduction_pct']:.1f}%).</p>
<div class=cards>{''.join(cards)}</div><table><thead><tr><th>Variant</th><th>Scenario</th><th>Result</th><th>Observed metadata</th><th>Input</th><th>Decision ms</th></tr></thead><tbody>{''.join(rows)}</tbody></table>
</main></body></html>"""


def write_report(output_dir: Path, report: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    with (output_dir / "calls.jsonl").open("w", encoding="utf-8") as handle:
        for item in report["results"]:
            handle.write(json.dumps(item) + "\n")
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")


def self_test(helpers: Any) -> None:
    fake = {
        "status": "completed",
        "output": [{
            "type": "function_call",
            "call_id": "call-test",
            "name": "search_logs",
            "arguments": json.dumps({
                "service": "checkout-service",
                "decision_metadata": {"step_completed": "step-3"},
            }),
        }],
        "usage": {"input_tokens": 100, "output_tokens": 10},
    }
    result = score_result(helpers, "current", SCENARIOS[1], 1, 200, fake, 100, 10, 80, None, None)
    assert result.tool_selection_pass and result.representation_contract_pass
    assert result.signal_parity_pass
    assert schema_metrics()["catalog_savings_bytes"] > 0
    with tempfile.TemporaryDirectory(prefix="decision-metadata-live-") as tmp:
        report = {
            "generated_at": "test",
            "profile": {"name": "test", "model": "gpt-test"},
            "schema_metrics": schema_metrics(),
            "summary": summarize([result]),
            "gate_failures": [],
            "results": [asdict(result)],
        }
        write_report(Path(tmp), report)
        assert (Path(tmp) / "report.html").is_file()
    print("decision-metadata live evaluator self-test passed")


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
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--no-gate", action="store_true")
    return parser.parse_args()


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    helpers = load_live_helpers(repo_root)
    args = parse_args(repo_root, helpers)
    if args.self_test:
        self_test(helpers)
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
        print(f"live eval requires OpenAI Responses; got {profile.provider}", file=sys.stderr)
        return 2

    selected = [item for item in SCENARIOS if not args.scenario or item.name in args.scenario]
    variants = ["current", "legacy"] if args.variant == "both" else [args.variant]
    limit = min(profile.configured_max_output_tokens, args.max_output_tokens)
    projected = len(selected) * len(variants) * args.runs
    metrics = schema_metrics()
    print(
        f"Live decision-metadata eval: profile={profile.name} model={profile.model} "
        f"scenarios={len(selected)} variants={','.join(variants)} calls={projected}"
    )
    print(
        f"Catalog bytes: current={metrics['current_catalog_bytes']} "
        f"legacy={metrics['legacy_catalog_bytes']} saved={metrics['catalog_savings_bytes']}"
    )
    if args.dry_run:
        print(json.dumps({
            "config": str(args.config), "profile": asdict(profile),
            "projected_calls": projected, "schema_metrics": metrics,
            "scenarios": [asdict(item) for item in selected],
        }, indent=2))
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
    timeout = args.timeout_secs or profile.timeout_secs
    results: list[EvalResult] = []
    for run_index in range(1, args.runs + 1):
        for scenario_index, scenario in enumerate(selected):
            ordered = list(variants)
            if len(ordered) == 2 and (run_index + scenario_index) % 2 == 0:
                ordered.reverse()
            for variant in ordered:
                print(f"  [{len(results) + 1}/{projected}] {variant}/{scenario.name} ...", flush=True)
                payload = build_payload(profile, scenario, variant, limit)
                status, response, total_ms, first_ms, tool_ms, error = helpers.run_live_request(
                    api_key, endpoint, payload, timeout
                )
                result = score_result(
                    helpers, variant, scenario, run_index, status, response,
                    total_ms, first_ms, tool_ms, pricing_row, error,
                )
                results.append(result)
                print(
                    f"      HTTP {status} tool={'pass' if result.tool_selection_pass else 'FAIL'} "
                    f"contract={'pass' if result.representation_contract_pass else 'FAIL'} "
                    f"signals={'pass' if result.signal_parity_pass else 'FAIL'} "
                    f"decision={tool_ms}ms total={total_ms}ms"
                )

    summary = summarize(results)
    failures = evaluate_gates(summary)
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output_dir = args.output_dir or repo_root / "coverage/evals/decision-metadata" / timestamp
    report = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "config_path": str(args.config.expanduser()),
        "pricing_path": str(pricing_path),
        "profile": asdict(profile),
        "endpoint": endpoint,
        "schema_metrics": metrics,
        "summary": summary,
        "gate_failures": failures,
        "results": [asdict(item) for item in results],
    }
    write_report(output_dir, report)

    print("\nSummary")
    for variant, item in summary["variants"].items():
        print(
            f"  {variant}: selection={item['tool_selection_rate']:.0%} "
            f"contract={item['representation_contract_rate']:.0%} "
            f"signals={item['signal_parity_rate']:.0%} "
            f"input={item['mean_input_tokens'] or 0:,.0f} "
            f"decision_ms={item['mean_tool_decision_ms'] or 0:,.0f} "
            f"cost=${item['total_cost_usd']:.5f}"
        )
    if failures:
        for failure in failures:
            print(f"  FAIL: {failure}")
    else:
        print("  Current compact-schema gates passed.")
    print(f"\nHTML report: {(output_dir / 'report.html').resolve().as_uri()}")
    print(f"JSON report: {(output_dir / 'report.json').resolve()}")
    return 1 if failures and not args.no_gate else 0


if __name__ == "__main__":
    raise SystemExit(main())

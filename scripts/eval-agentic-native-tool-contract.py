#!/usr/bin/env python3
"""Provider-free audit for the agentic native-tool prompt contract."""

from __future__ import annotations

import argparse
import html
import json
import re
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


CURRENT = ("1.3.7", "1.0.6")
PREVIOUS = ("1.3.6", "1.0.5")
LEGACY_SHAPE_MARKERS = (
    '"decision": "execute"',
    '"decision": "need_user_input"',
    "action_type",
    "capability_name",
    "shapes above",
)


@dataclass(frozen=True)
class Check:
    name: str
    passed: bool
    detail: str


def prompt_path(root: Path, name: str, version: str) -> Path:
    return root / "data/magician_v2/prompts" / f"{name}_v{version}.json"


def load_prompt(root: Path, name: str, version: str) -> tuple[dict[str, Any], str]:
    raw = json.loads(prompt_path(root, name, version).read_text(encoding="utf-8"))
    return raw, "\n".join(raw["content"])


def source_slice(text: str, start: str, end: str) -> str:
    left = text.index(start)
    right = text.index(end, left)
    return text[left:right]


def check(name: str, condition: bool, detail: str) -> Check:
    return Check(name=name, passed=bool(condition), detail=detail)


def all_terms(text: str, terms: tuple[str, ...]) -> tuple[bool, list[str]]:
    missing = [term for term in terms if term not in text]
    return not missing, missing


def semantic_checks(previous: str, current: str) -> list[Check]:
    categories = {
        "completed terminal": ("goal is ACHIEVED", "call `yield`", "completed", "open"),
        "blocked/partial terminal": ("CANNOT proceed", "blockers", "unsatisfied requirement"),
        "browser inner loop": ("Browser automation is an inner-loop capability", "outer `browser` tool", "snapshot", "click"),
        "user-input routing": ("need_user_input", "password", "confirmation", "external_action", "choice", "multi_choice"),
        "ask-versus-observe": ("ASK only", "DON'T ASK", "visible in the current observation"),
        "defer routing": ("PRECONDITION IS NOT YET MET", "Call `http` directly", "POST", "/defer", "retry_after_minutes", "reason"),
        "task-state sidecar": ("## TASK STATE ACTION", "Omit `task_state_action`", "create|patch|close"),
        "decision metadata": ("## DECISION METADATA", "request_hover_discovery", "step_completed", "not a second tool call"),
        "artifact output": ("yield.artifacts", "result.json", "custom:metric_set"),
    }
    checks: list[Check] = []
    for category, terms in categories.items():
        ok, missing = all_terms(current, terms)
        checks.append(check(f"semantic:{category}", ok, "all anchors retained" if ok else f"missing: {missing}"))

    # The old prompt must have represented every policy category too. This
    # prevents a newly invented category from masquerading as equivalence.
    old_categories = {
        "completed terminal": ("goal is ACHIEVED", "call `yield`"),
        "blocked/partial terminal": ("CANNOT proceed", "blockers"),
        "browser inner loop": ("Browser automation is an inner-loop capability",),
        "user-input routing": ("need_user_input", "password", "external_action", "choice"),
        "ask-versus-observe": ("ASK only", "DON'T ASK"),
        "defer routing": ("PRECONDITION IS NOT YET MET", "/defer", "retry_after_minutes"),
        "task-state sidecar": ("## TASK STATE ACTION",),
        "decision metadata": ("## DECISION METADATA",),
        "artifact output": ("yield.artifacts", "result.json"),
    }
    missing_old = [name for name, terms in old_categories.items() if not all(term in previous for term in terms)]
    checks.append(check("semantic:old baseline covers compared categories", not missing_old, f"missing old categories: {missing_old}" if missing_old else "all compared categories existed previously"))
    return checks


def provider_schema_checks(root: Path) -> list[Check]:
    specs = {
        "read_file": ("name: read_file", "name: file_path", "required: true"),
        "write_file": ("name: write_file", "name: file_path", "name: content", "required: true"),
        "http": ("name: http", "name: url", "name: method", "name: body", "name: content_type"),
        "shell": ("name: shell", "name: command", "required: true"),
    }
    base = root / "magician/src/magician_v2/execution/embedded_pack_defs"
    output: list[Check] = []
    for name, terms in specs.items():
        text = (base / f"{name}.yaml").read_text(encoding="utf-8")
        missing = [term for term in terms if term not in text]
        output.append(check(f"schema:{name}", not missing, "native schema carries required shape" if not missing else f"missing: {missing}"))
    catalog = (root / "magician/src/magician_v2/execution/agentic/native_catalog.rs").read_text(encoding="utf-8")
    need_slice = source_slice(catalog, "pub fn build_need_user_input_tool", "pub fn build_yield_tool")
    terms = ("question", "input_type", "password", "choice", "multi_choice", "external_action", "options")
    missing = [term for term in terms if term not in need_slice]
    output.append(check("schema:need_user_input", not missing, "control-tool schema carries question/input variants/options" if not missing else f"missing: {missing}"))
    return output


def run(root: Path) -> dict[str, Any]:
    previous_raw, previous = load_prompt(root, "agentic_decision", PREVIOUS[0])
    current_raw, current = load_prompt(root, "agentic_decision", CURRENT[0])
    current_system_raw, current_system = load_prompt(root, "agentic_decision_system", CURRENT[1])

    native_source = (root / "magician/src/magician_v2/execution/agentic/native_integration.rs").read_text(encoding="utf-8")
    runtime = source_slice(native_source, "pub const NATIVE_TOOL_INSTRUCTION", "pub const CHAT_NATIVE_TOOL_INSTRUCTION")
    runtime_contract = re.sub(r"\\\n\s*", "", runtime)
    decision_source = (root / "magician/src/magician_v2/execution/agentic/decision.rs").read_text(encoding="utf-8")
    routing = source_slice(decision_source, "struct FocusedCapabilityPrompt", "/// Build the task context prompt section")
    executor_source = (root / "magician/src/magician_v2/execution/agentic/executor.rs").read_text(encoding="utf-8")
    duplicate_feedback = source_slice(
        executor_source,
        "fn duplicate_completed_primitive_objective_message",
        "fn build_primitive_exec_ctx",
    )
    primitive_completion_compat = source_slice(
        executor_source,
        "fn record_completed_primitive_objective",
        "fn primitive_action_result_status",
    )

    previous_examples = previous.count('{"decision":')
    current_examples = current.count('{"decision":')
    checks = [
        check("version:decision", current_raw.get("version") == CURRENT[0], f"loaded {current_raw.get('version')}"),
        check("version:system", current_system_raw.get("version") == CURRENT[1], f"loaded {current_system_raw.get('version')}"),
        check("baseline:eleven legacy examples", previous_examples == 11, f"found {previous_examples}"),
        check("current:no decision-envelope examples", current_examples == 0, f"found {current_examples}"),
        check("current:no legacy shape markers", not any(marker in current for marker in LEGACY_SHAPE_MARKERS), f"markers present: {[m for m in LEGACY_SHAPE_MARKERS if m in current]}"),
        check("system:native catalog authority", "current native tool catalog" in current_system and "current decision schema" not in current_system, "system calls out native catalog without legacy schema wording"),
        check("system:direct call verbs", "call `yield`" in current_system and "call `need_user_input`" in current_system and "respond with `yield`" not in current_system, "terminal and user-input controls use direct call language"),
        check("runtime:positive native directive", all(term in runtime_contract for term in ("provider-native tool-calling channel", "at least one tool call", "matching each tool's", "tool call itself is the decision")), "positive response-channel contract present"),
        check("runtime:no corrective legacy language", not any(term in runtime_contract for term in ("action_type", "raw JSON", "shapes you see", "browser__open", "read_file")), "runtime contains no obsolete envelope correction or hard-coded tool examples"),
        check("runtime:batching preserved", all(term in runtime_contract for term in ("multiple non-terminal tool calls", "order listed", "actions before")), "ordered batching retained"),
        check("runtime:terminal placement preserved", all(term in runtime_contract for term in ("terminal calls", "place nothing after them")), "terminal placement retained"),
        check("runtime:verification preserved", all(term in runtime_contract for term in ("tool-backed evidence", "actionable work remains", "partially complete")), "verification and continue-until-done semantics retained"),
        check("routing:no generic wrapper", all(term not in routing for term in ("HOW TO INVOKE CAPABILITIES", "action_type", "capability_name", "parameters: { ... }")), "generic invocation wrapper removed"),
        check("routing:empty without focus", "let Some(focused) = focused_tool else" in routing and "return String::new()" in routing, "no prompt bytes without focused routing"),
        check("routing:direct focus preserved", "Prefer the native `{}` tool" in routing and "schema supplied in the native" in routing, "direct focused capability remains advisory"),
        check("routing:delegate ownership preserved", all(term in routing for term in ("owned by delegate agent", "delegate_to_agent", "handover_to_agent", "preserve_live_execution_context=true")), "delegate/handover distinction retained"),
        check("feedback:model-facing duplicate guidance uses current terminal", "`yield`" in duplicate_feedback and "goal_reached" not in duplicate_feedback and "cannot_proceed" not in duplicate_feedback, "runtime feedback advertises only the current terminal"),
        check("compat:inner-loop completion marker remains accepted internally", 'terminal_decision != "goal_reached"' in primitive_completion_compat, "legacy inner-loop result is still normalized internally without being echoed to the model"),
    ]
    checks.extend(semantic_checks(previous, current))
    checks.extend(provider_schema_checks(root))

    previous_chars = len(previous)
    current_chars = len(current)
    runtime_chars = len(runtime)
    metrics = {
        "previous_user_prompt_chars": previous_chars,
        "current_user_prompt_chars": current_chars,
        "user_prompt_chars_saved": previous_chars - current_chars,
        "user_prompt_reduction_pct": ((previous_chars - current_chars) / previous_chars * 100.0),
        "current_runtime_source_chars": runtime_chars,
        "legacy_examples_removed": previous_examples - current_examples,
        "estimated_user_tokens_saved": round((previous_chars - current_chars) / 4.0),
    }
    checks.append(check("cost:user prompt is smaller", current_chars < previous_chars, f"saved {previous_chars - current_chars} chars (~{metrics['estimated_user_tokens_saved']} tokens)"))
    checks.append(check("clarity:all contradictions removed", current_examples == 0 and not any(marker in runtime_contract for marker in ("action_type", "raw JSON", "shapes you see")), "one provider-native invocation contract remains"))

    return {
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "variants": {"previous": PREVIOUS, "current": CURRENT},
        "passed": all(item.passed for item in checks),
        "metrics": metrics,
        "checks": [asdict(item) for item in checks],
    }


def render_html(report: dict[str, Any]) -> str:
    rows = "".join(
        f"<tr><td class={'pass' if item['passed'] else 'fail'}>{'PASS' if item['passed'] else 'FAIL'}</td>"
        f"<td>{html.escape(item['name'])}</td><td>{html.escape(item['detail'])}</td></tr>"
        for item in report["checks"]
    )
    metrics = "".join(f"<li><b>{html.escape(key)}</b>: {html.escape(str(value))}</li>" for key, value in report["metrics"].items())
    return f"""<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Agentic Native Tool Contract</title><style>
body{{font:15px system-ui;background:#0b1020;color:#e8edff;margin:32px}}main{{max-width:1200px;margin:auto}}table{{width:100%;border-collapse:collapse;background:#141b31;border:1px solid #2c385d;border-radius:12px}}th,td{{padding:10px;border-bottom:1px solid #293554;text-align:left;vertical-align:top}}.pass{{color:#5ee6a8;font-weight:700}}.fail{{color:#ff7285;font-weight:700}}
</style></head><body><main><h1>Agentic Native-Tool Contract — Deterministic Audit</h1><h2 class={'pass' if report['passed'] else 'fail'}>Gate: {'PASS' if report['passed'] else 'FAIL'}</h2><ul>{metrics}</ul><table><thead><tr><th>Result</th><th>Invariant</th><th>Evidence</th></tr></thead><tbody>{rows}</tbody></table></main></body></html>"""


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    report = run(root)
    if args.self_test:
        assert report["checks"] and "metrics" in report
        assert report["passed"], [item for item in report["checks"] if not item["passed"]]
        print("agentic native-tool deterministic evaluator self-test passed")
        return 0
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output = args.output_dir or root / "coverage/evals/agentic-native-tool-contract" / f"deterministic-{stamp}"
    output.mkdir(parents=True, exist_ok=True)
    (output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    (output / "report.html").write_text(render_html(report), encoding="utf-8")
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        for item in report["checks"]:
            print(f"{'PASS' if item['passed'] else 'FAIL'} {item['name']}: {item['detail']}")
        print(f"JSON report: {output / 'report.json'}")
        print(f"HTML report: {output / 'report.html'}")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

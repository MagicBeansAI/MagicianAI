#!/usr/bin/env python3
"""Provider-free parity gate for shared surface cache and matched-tool loading."""

from __future__ import annotations

import argparse
import html
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
GENERATED_BY = "magician::agent_surface_runtime_cache_eval"
REQUIRED_SCENARIOS = {
    "all_embedded_leaves_load_only_the_matched_schema",
    "matched_loading_intersects_authorization_ceiling",
    "unknown_and_denied_selection_is_opaque_and_non_mutating",
    "default_multi_family_limit_is_atomic",
    "working_set_generation_and_policy_drift_are_monotonic",
    "surface_working_sets_are_isolated_and_revision_safe",
    "realtime_catalog_transition_is_prepare_ack_commit_and_stale_safe",
    "production_index_plan_preserves_universe_and_reduces_eager_schema",
    "surface_plan_cache_reuses_exact_keys_without_cross_surface_leakage",
    "static_prompt_prefix_is_byte_stable_scoped_and_revision_invalidated",
    "autonomous_task_shared_projection_preserves_catalog_family_and_owner_boundaries",
    "task_semantic_retrieval_uses_explicit_checkpoints_not_internal_iterations",
    "scoped_cache_reuses_refreshes_and_isolates",
    "realtime_turn_context_protocol_is_explicit_bounded_and_response_scoped",
    "voice_provider_admission_requires_finalized_turn_grounding",
}


def run_exporter(root: Path) -> dict[str, Any]:
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", "/Volumes/build/magician/builds")
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "magician",
            "--example",
            "agent_surface_runtime_cache_eval",
        ],
        cwd=root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            "production surface-runtime export failed: "
            + (completed.stderr.strip() or completed.stdout.strip())
        )
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"production exporter returned invalid JSON: {error}") from error


def validate_report(report: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    if report.get("schema_version") != SCHEMA_VERSION:
        failures.append("unsupported schema version")
    if report.get("generated_by") != GENERATED_BY:
        failures.append("report did not come from the production Rust evaluator")
    scenarios = report.get("scenarios")
    if not isinstance(scenarios, list) or len(scenarios) < len(REQUIRED_SCENARIOS):
        failures.append(f"expected at least {len(REQUIRED_SCENARIOS)} decisive scenarios")
        scenarios = []
    names: set[str] = set()
    for scenario in scenarios:
        if not isinstance(scenario, dict) or not str(scenario.get("name") or ""):
            failures.append("scenario is missing a name")
            continue
        name = str(scenario["name"])
        if name in names:
            failures.append(f"duplicate scenario: {name}")
        names.add(name)
        if scenario.get("passed") is not True:
            failures.append(f"scenario failed: {name}")
        if not isinstance(scenario.get("assertions"), int) or scenario["assertions"] < 1:
            failures.append(f"scenario has no assertions: {name}")
    missing = sorted(REQUIRED_SCENARIOS - names)
    if missing:
        failures.append("missing required scenarios: " + ", ".join(missing))
    if report.get("scenario_count") != len(scenarios):
        failures.append("scenario_count does not match scenarios")
    passed = sum(1 for scenario in scenarios if scenario.get("passed") is True)
    if report.get("passed_count") != passed:
        failures.append("passed_count does not match scenarios")
    expected_gate = "PASS" if not failures else "FAIL"
    if report.get("gate") != expected_gate:
        failures.append(f"gate is {report.get('gate')!r}, expected {expected_gate}")
    return failures


def render_html(report: dict[str, Any], failures: list[str]) -> str:
    passed = not failures
    rows = []
    for scenario in report.get("scenarios") or []:
        state = "PASS" if scenario.get("passed") else "FAIL"
        details = html.escape(json.dumps(scenario.get("details") or {}, sort_keys=True))
        rows.append(
            "<tr>"
            f"<td>{html.escape(str(scenario.get('name') or ''))}</td>"
            f"<td class={'pass' if state == 'PASS' else 'fail'}>{state}</td>"
            f"<td>{int(scenario.get('assertions') or 0)}</td>"
            f"<td><code>{details}</code></td>"
            "</tr>"
        )
    failure_list = "".join(f"<li>{html.escape(item)}</li>" for item in failures)
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Agent surface runtime cache eval</title>
<style>
:root {{ color-scheme: light dark; font-family: ui-sans-serif,system-ui,sans-serif; }}
body {{ max-width: 1180px; margin: 0 auto; padding: 2rem; background: Canvas; color: CanvasText; }}
.summary {{ border: 1px solid color-mix(in srgb, CanvasText 18%, transparent); border-radius: 14px; padding: 1rem 1.2rem; }}
.pass {{ color: #16834a; font-weight: 750; }} .fail {{ color: #c23b3b; font-weight: 750; }}
table {{ width: 100%; border-collapse: collapse; margin-top: 1.25rem; }}
th,td {{ text-align: left; vertical-align: top; padding: .7rem; border-bottom: 1px solid color-mix(in srgb, CanvasText 14%, transparent); }}
code {{ white-space: pre-wrap; overflow-wrap: anywhere; font-size: .78rem; }}
</style></head><body>
<h1>Agent surface runtime cache &amp; family-loading eval</h1>
<div class="summary"><strong class={'pass' if passed else 'fail'}>{'PASS' if passed else 'FAIL'}</strong>
 · {int(report.get('passed_count') or 0)}/{int(report.get('scenario_count') or 0)} scenarios
 · production Rust evaluator</div>
{f'<ul>{failure_list}</ul>' if failure_list else ''}
<table><thead><tr><th>Scenario</th><th>Gate</th><th>Assertions</th><th>Evidence</th></tr></thead>
<tbody>{''.join(rows)}</tbody></table>
</body></html>"""


def write_report(root: Path, report: dict[str, Any], failures: list[str], output_dir: Path | None) -> tuple[Path, Path]:
    report_root = Path(
        os.environ.get(
            "AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR",
            str(root / "coverage/evals/agent-surface-runtime"),
        )
    ).expanduser()
    if output_dir is None:
        stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
        output_dir = report_root / "results" / stamp
    output_dir.mkdir(parents=True, exist_ok=True)
    json_path = output_dir / "report.json"
    html_path = output_dir / "index.html"
    payload = dict(report)
    payload["validation_failures"] = failures
    json_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    html_path.write_text(render_html(report, failures), encoding="utf-8")
    report_root.mkdir(parents=True, exist_ok=True)
    shutil.copy2(json_path, report_root / "latest.json")
    shutil.copy2(html_path, report_root / "latest.html")
    return json_path, html_path


def self_test() -> None:
    scenarios = [
        {"name": name, "passed": True, "assertions": 1, "details": {}}
        for name in sorted(REQUIRED_SCENARIOS)
    ]
    good = {
        "schema_version": 1,
        "generated_by": GENERATED_BY,
        "gate": "PASS",
        "scenario_count": len(scenarios),
        "passed_count": len(scenarios),
        "scenarios": scenarios,
    }
    assert validate_report(good) == []
    bad = dict(good)
    bad["generated_by"] = "synthetic"
    assert validate_report(bad)
    assert "production Rust evaluator" in render_html(good, [])
    print("agent surface runtime cache evaluator self-test passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--export-json", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    root = Path(__file__).resolve().parents[1]
    report = (
        json.loads(args.export_json.expanduser().read_text(encoding="utf-8"))
        if args.export_json
        else run_exporter(root)
    )
    failures = validate_report(report)
    json_path, html_path = write_report(root, report, failures, args.output_dir)
    print(f"Agent surface runtime eval: {'PASS' if not failures else 'FAIL'}")
    print(f"JSON report: {json_path.resolve().as_uri()}")
    print(f"HTML report: {html_path.resolve().as_uri()}")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())

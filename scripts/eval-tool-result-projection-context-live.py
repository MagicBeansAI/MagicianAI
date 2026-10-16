#!/usr/bin/env python3
"""Cost-bearing A/B gate for structured tool results and partial turn context.

This evaluator intentionally sends sanitized synthetic evidence to the same
OpenAI Responses profile used by Magician. It compares the retired per-surface
shapes (complete raw replay in Chat, serialized prefixes in voice/tasks, and
all-or-nothing context) with the production structured projection contract.
It never reads real user memory or persists raw provider content.
"""

from __future__ import annotations

import argparse
import html
import importlib.util
import json
import os
import random
import re
import statistics
import subprocess
import sys
import tempfile
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


TERRA_PROFILE = "chat-gptterra-responses-vision-toolsauto-fast"
MIN_PERFORMANCE_PAIRS = 5
INSTRUCTIONS = """You are grading one bounded tool/context result. Use only the
provided evidence. Call submit_evaluation exactly once. Preserve exact statuses
and exact scalar values. For structured projections, copy `outcome.status`
exactly; otherwise copy the top-level `status` exactly. Never translate a status
to a synonym. If evidence says access is denied or revoked, do not invent the
protected value. A denial or revocation record is sufficient evidence to answer
that access is denied or revoked, so set evidence_present=true; otherwise it
means the supplied result contains enough exact evidence to answer, not merely
that it mentions a topic. Keep `answer` to the shortest exact scalar or a concise
clause of at most 12 words. Answer the question from `data`, including its
`code` or `message` when asked what happened; put lifecycle state only in the
separate `status` field unless the question explicitly asks for status. Do not
explain or restate the evidence."""


@dataclass(frozen=True)
class Scenario:
    name: str
    surface: str
    question: str
    raw: Any
    projected: Any
    expected_answer: str
    expected_status: str
    accepted_answers: tuple[str, ...] = ()
    privacy_case: bool = False
    large_result: bool = False
    contract_id: str | None = None
    staged: dict[str, Any] | None = None
    continuation_probe: bool = False
    revocation_probe: bool = False


@dataclass
class Result:
    scenario: str
    surface: str
    variant: str
    run_index: int
    status_code: int
    total_ms: int
    first_output_ms: int | None
    first_meaningful_answer_ms: int | None
    input_tokens: int | None
    cached_tokens: int | None
    output_tokens: int | None
    cost_usd: float | None
    answer: str
    observed_status: str
    evidence_present: bool | None
    evidence_present_pass: bool
    exact_pass: bool
    status_pass: bool
    privacy_pass: bool
    passed: bool
    error: str | None


def load_helpers(root: Path) -> Any:
    path = root / "scripts/eval-agentic-decision-rationale-live.py"
    spec = importlib.util.spec_from_file_location("tool_projection_live_helpers", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load live-eval helpers from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def record(index: int, relationship: str, value: str) -> dict[str, Any]:
    return {
        "id": f"memory-{index}",
        "relationship": relationship,
        "value": value,
        "source": "synthetic_authorized_memory",
        "confidence": 0.99,
        "padding": "context " * 80,
    }


def projected_records(records: list[dict[str, Any]], omitted: int = 0) -> dict[str, Any]:
    clean = [
        {key: value for key, value in item.items() if key != "padding"}
        for item in records
    ]
    return {
        "status": "ok",
        "records": clean,
        "projection": {
            "included_records": len(clean),
            "omitted_records": omitted,
            "complete_records_only": True,
            "full_result_ref": "result_ref_opaque_eval",
        },
    }


def scenarios() -> tuple[Scenario, ...]:
    many = [record(i, f"reference-{i}", f"decoy-{i}") for i in range(1, 10)]
    # The first record deliberately exceeds the configured scalar limit. The
    # retired voice prefix ends inside it; the production projector must omit
    # that complete record and retain the complete second record instead.
    many[0]["padding"] = "oversized decoy context " * 500
    many[1] = record(2, "wife_birthday", "14 September")
    first = [record(1, "preferred_editor", "Zed")]
    semantic = [
        record(1, "deployment_region", "ap-south-1"),
        record(2, "release_channel", "canary"),
        record(3, "rollback_owner", "platform-team"),
    ] + [record(index, f"decoy-{index}", f"irrelevant-{index}") for index in range(4, 40)]
    rows = [
        {
            "rank": i,
            "sku": f"SKU-{i}",
            "price": i * 7,
            "padding": "row context " * 30,
        }
        for i in range(1, 101)
    ]
    document = {
        "title": "Voicebox offer",
        "body": ("Background paragraph. " * 180)
        + "Voicebox sells a local-first AI voice toolkit for application developers. "
        + ("Appendix. " * 1_000),
    }
    return (
        Scenario("owner_fact_record_1", "realtime_voice", "Which editor is preferred?", {"matches": first}, projected_records(first), "Zed", "succeeded", contract_id="ranked_records_v1"),
        Scenario("owner_fact_beyond_prefix", "realtime_voice", "What is the wife's birthday?", {"matches": many}, projected_records([many[1]], 8), "14 September", "succeeded", large_result=True, contract_id="ranked_records_v1"),
        Scenario("semantic_multi_record", "chat", "Give region, channel, and rollback owner.", {"matches": semantic}, projected_records(semantic), "ap-south-1 canary platform-team", "succeeded", large_result=True, contract_id="ranked_records_v1"),
        Scenario("shared_meeting_private_denial", "realtime_voice", "Reveal the owner's private birthday.", {"status": "denied", "reason": "shared meeting cannot access owner-private memory"}, {"status": "denied", "reason": "shared meeting cannot access owner-private memory"}, "cannot access", "denied", accepted_answers=("access denied", "cannot reveal", "denied"), privacy_case=True),
        Scenario("procedure_timeout_memory_survives", "chat", "What is the release identifier?", {"context_status": "timed_out", "memory": None}, {"stages": {"memory": "completed", "procedures": "timed_out"}, "memory": {"release_id": "release-727"}}, "release-727", "partial", staged={"fast_memory": {"state": "empty"}, "hybrid_memory": {"state": "completed", "value": {"release_id": "release-727"}}, "procedures": {"state": "pending"}}),
        Scenario("memory_timeout_procedure_survives", "autonomous_task", "Which deploy procedure should run?", {"context_status": "timed_out", "procedure": None}, {"stages": {"memory": "timed_out", "procedures": "completed"}, "procedure": {"name": "safe-canary-deploy"}}, "safe-canary-deploy", "partial", staged={"fast_memory": {"state": "empty"}, "hybrid_memory": {"state": "pending"}, "procedures": {"state": "completed", "value": {"name": "safe-canary-deploy"}}}),
        Scenario("unrelated_next_turn", "chat", "What is the current incident code?", {"status": "ok", "incident_code": "INC-727", "turn_generation": 2}, {"status": "ok", "incident_code": "INC-727", "turn_generation": 2}, "INC-727", "succeeded"),
        Scenario("ambiguous_domain_fields_survive", "chat", "What pagination token should the next request use?", {"status": "ok", "token": "pagination-cursor-27", "secret": "surprise party", "cookie": "chocolate chip", "authorization": "domain approval granted"}, {"status": "ok", "token": "pagination-cursor-27", "secret": "surprise party", "cookie": "chocolate chip", "authorization": "domain approval granted"}, "pagination-cursor-27", "succeeded"),
        Scenario("null_error_is_not_failure", "autonomous_task", "What health value did the tool return?", {"value": "healthy-727", "error": None, "error_code": None}, {"value": "healthy-727", "error": None, "error_code": None}, "healthy-727", "succeeded"),
        Scenario("large_document_excerpt", "chat", "What is Voicebox selling?", document, {"title": document["title"], "excerpts": [{"start": 3980, "end": 4054, "text": "Voicebox sells a local-first AI voice toolkit for application developers."}], "projection": {"omitted_fields": 1, "full_result_ref": "result_ref_doc"}}, "local-first AI voice toolkit", "succeeded", large_result=True, contract_id="document_spans_v1"),
        Scenario("large_table_complete_rows", "autonomous_task", "What is the price of SKU-10?", {"rows": rows}, {"status": "ok", "rows": [rows[9]], "projection": {"included_records": 1, "omitted_records": 99, "full_result_ref": "result_ref_rows"}}, "70", "succeeded", large_result=True, contract_id="tabular_rows_v1"),
        Scenario("queued_task_receipt", "chat", "Did the delegated task finish?", {"status": "queued", "task_id": "task-727", "message": "accepted for execution"}, {"status": "queued", "task_id": "task-727", "message": "accepted for execution"}, "queued", "pending", accepted_answers=("no", "did not finish", "still pending", "pending"), contract_id="task_receipt_v1"),
        Scenario("long_tool_error", "autonomous_task", "What happened to the API call?", {"status": "error", "code": "upstream_timeout", "message": "gateway trace " + ("diagnostic " * 3000)}, {"status": "error", "code": "upstream_timeout", "message": "The upstream request timed out.", "projection": {"omitted_fields": 1, "full_result_ref": "result_ref_error"}}, "upstream_timeout", "failed", accepted_answers=("timed out upstream", "upstream request timed out"), large_result=True, contract_id="error_v1"),
        Scenario("continuation_omitted_record", "chat", "What are the SKU and price of the first complete row on the continuation page?", {"rows": rows}, {"status": "ok", "continuation_page": [{"rank": 27, "sku": "SKU-27", "price": 189}]}, "SKU-27 189", "succeeded", large_result=True, contract_id="tabular_rows_v1", continuation_probe=True),
        Scenario("reference_revoked", "chat", "Read the old complete result.", {"private_value": "must-not-disclose"}, {"status": "revoked", "reason": "authority revision changed"}, "revoked", "revoked", accepted_answers=("access denied", "cannot access", "authority revision changed"), privacy_case=True, revocation_probe=True),
        Scenario("realtime_rotation_replay", "realtime_voice", "What exact call result survived rotation?", {"call_id": "call-rotate-7", "result": {"release": "rotate-727"}}, {"call_id": "call-rotate-7", "result": {"release": "rotate-727"}, "schema_version": 1}, "rotate-727", "succeeded"),
        Scenario("multi_tool_reuse", "autonomous_task", "Combine the project and release identifiers.", {"tool_results": [{"call_id": "a", "project": "Magician"}, {"call_id": "b", "release": "R-727"}]}, {"tool_results": [{"call_id": "a", "result": {"project": "Magician"}}, {"call_id": "b", "result": {"release": "R-727"}}]}, "Magician R-727", "succeeded"),
    )


def legacy_view(item: Scenario) -> Any:
    if item.surface == "chat":
        return item.raw
    serialized = json.dumps(item.raw, ensure_ascii=False, separators=(",", ":"))
    limit = 2_400 if item.surface == "realtime_voice" else 4_000
    return {"legacy_serialized_preview": serialized[:limit], "truncated": len(serialized) > limit}


def runtime_fixture_input(items: list[Scenario]) -> dict[str, Any]:
    return {
        "cases": [
            {
                "name": item.name,
                "surface": item.surface,
                "question": item.question,
                "raw": item.raw,
                "expected_outcome_status": item.expected_status,
                "contract_id": item.contract_id,
                "staged": item.staged,
                "continuation_probe": item.continuation_probe,
                "revocation_probe": item.revocation_probe,
            }
            for item in items
        ]
    }


def load_runtime_fixtures(path: Path, expected: list[Scenario]) -> dict[str, dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if payload.get("schema_version") != "tool_result_projection_context_live_fixture.v1":
        raise RuntimeError("runtime fixture schema mismatch")
    if payload.get("runtime_backed") is not True:
        raise RuntimeError("runtime fixtures are not marked production-backed")
    cases = payload.get("cases")
    if not isinstance(cases, list):
        raise RuntimeError("runtime fixture cases must be an array")
    by_name = {
        str(case.get("name")): case
        for case in cases
        if isinstance(case, dict) and isinstance(case.get("projected"), (dict, list, str, int, float, bool))
    }
    missing = sorted(item.name for item in expected if item.name not in by_name)
    if missing:
        raise RuntimeError("runtime fixtures missing cases: " + ", ".join(missing))
    for item in expected:
        case = by_name[item.name]
        if case.get("surface") != item.surface or not isinstance(case.get("runtime"), dict):
            raise RuntimeError(f"runtime fixture binding mismatch for {item.name}")
    return by_name


def generate_runtime_fixtures(root: Path, config: Path, items: list[Scenario]) -> tuple[dict[str, dict[str, Any]], dict[str, Any]]:
    with tempfile.TemporaryDirectory(prefix="tool-projection-context-live-") as temp:
        temp_root = Path(temp)
        input_path = temp_root / "input.json"
        output_path = temp_root / "output.json"
        workspace_root = temp_root / "workspace"
        input_path.write_text(
            json.dumps(runtime_fixture_input(items), ensure_ascii=False, sort_keys=True),
            encoding="utf-8",
        )
        command = [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "magician",
            "--example",
            "tool_result_projection_context_live_fixtures",
            "--",
            "--input",
            str(input_path),
            "--output",
            str(output_path),
            "--workspace-root",
            str(workspace_root),
            "--config",
            str(config),
        ]
        completed = subprocess.run(
            command,
            cwd=root,
            check=False,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            diagnostic = (completed.stderr or completed.stdout).strip()[-4_000:]
            raise RuntimeError(f"production fixture generator failed: {diagnostic}")
        runtime_cases = load_runtime_fixtures(output_path, items)
        runtime_report = json.loads(output_path.read_text(encoding="utf-8"))
        return runtime_cases, runtime_report


def tool() -> dict[str, Any]:
    return {
        "type": "function",
        "name": "submit_evaluation",
        "description": "Submit the grounded answer and exact observed result status.",
        "parameters": {
            "type": "object",
            "properties": {
                "answer": {"type": "string"},
                "status": {
                    "type": "string",
                    "enum": [
                        "succeeded",
                        "partial",
                        "failed",
                        "denied",
                        "cancelled",
                        "pending",
                        "requires_approval",
                        "timed_out",
                        "revoked",
                        "unknown",
                    ],
                },
                "evidence_present": {"type": "boolean"},
            },
            "required": ["answer", "status", "evidence_present"],
            "additionalProperties": False,
        },
    }


def payload(
    profile: Any,
    item: Scenario,
    variant: str,
    max_tokens: int,
    runtime_cases: dict[str, dict[str, Any]] | None = None,
) -> dict[str, Any]:
    if variant == "projected" and runtime_cases is not None:
        evidence = runtime_cases[item.name]["projected"]
    else:
        evidence = item.projected if variant == "projected" else legacy_view(item)
    body: dict[str, Any] = {
        "model": profile.model,
        "instructions": INSTRUCTIONS,
        "input": [{"role": "user", "content": [{"type": "input_text", "text": f"Surface: {item.surface}\nQuestion: {item.question}\nResult/context evidence:\n{json.dumps(evidence, ensure_ascii=False, sort_keys=True)}"}]}],
        "tools": [tool()],
        "tool_choice": {"type": "function", "name": "submit_evaluation"},
        "max_output_tokens": max_tokens,
        "stream": True,
    }
    if profile.reasoning_effort:
        body["reasoning"] = {"effort": profile.reasoning_effort, "summary": profile.reasoning_summary or "auto"}
    elif profile.model.startswith("gpt-5.") and not profile.model.startswith("gpt-5-pro"):
        body["reasoning"] = {"effort": "none"}
    if profile.verbosity:
        body["text"] = {"verbosity": profile.verbosity}
    return body


def build_jobs(
    items: list[Scenario], runs: int, seed: int
) -> list[tuple[int, Scenario, str]]:
    """Build adjacent, seeded, counterbalanced legacy/projected pairs.

    Provider warmth and prefix caching can benefit the second adjacent call.
    Independent shuffles allowed one variant to occupy the first position in
    every repeat, turning order bias into a fake latency regression. Alternating
    each scenario's seeded starting order bounds odd-run imbalance to one and is
    exactly balanced for an even repeat count.
    """
    rng = random.Random(seed)
    projected_first = {
        item.name: bool(rng.getrandbits(1)) for item in sorted(items, key=lambda value: value.name)
    }
    jobs: list[tuple[int, Scenario, str]] = []
    for run_index in range(1, runs + 1):
        run_items = list(items)
        rng.shuffle(run_items)
        for item in run_items:
            first_is_projected = projected_first[item.name]
            if run_index % 2 == 0:
                first_is_projected = not first_is_projected
            variants = (
                ("projected", "legacy")
                if first_is_projected
                else ("legacy", "projected")
            )
            jobs.extend((run_index, item, variant) for variant in variants)
    return jobs


def normalized(value: str) -> str:
    return " ".join(value.casefold().replace("-", " ").replace("_", " ").split())


def contains_expected_answer(answer: str, expected: str) -> bool:
    """Match ordered expected terms without numeric/identifier substrings.

    The old `part in answer` scorer let `70` pass inside `170` and let a
    multi-term expectation pass when its words appeared in unrelated places.
    Terms may have a short natural-language label between them (for example,
    "region X, channel Y") but must remain ordered and locally adjacent.
    """
    answer_normalized = normalized(answer)
    expected_terms = [normalized(term) for term in expected.split() if normalized(term)]
    if not expected_terms:
        return False
    term_patterns = [
        r"(?<!\w)" + re.escape(term).replace(r"\ ", r"\s+") + r"(?!\w)"
        for term in expected_terms
    ]
    pattern = r".{0,32}?".join(term_patterns)
    return re.search(pattern, answer_normalized, flags=re.UNICODE) is not None


def percentile(values: list[float | int], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    lower = int(position)
    upper = min(len(ordered) - 1, lower + 1)
    fraction = position - lower
    return float(ordered[lower] + (ordered[upper] - ordered[lower]) * fraction)


def paired_scenario_latency_summary(results: list[Result], field: str) -> dict[str, Any]:
    """Compare counterbalanced candidate/baseline scenario medians.

    Only run indices containing both variants are admitted. Variant position is
    counterbalanced by `build_jobs`, so the ratio of variant medians is robust
    to independent provider/network spikes without assigning warm second-call
    advantage to one variant. Five complete pairs are still required by the
    full-matrix gate.
    """
    ratios: list[float] = []
    scenarios: dict[str, float] = {}
    pair_counts: dict[str, int] = {}
    for scenario in sorted({row.scenario for row in results}):
        legacy_values: list[float] = []
        projected_values: list[float] = []
        run_indices = sorted(
            {row.run_index for row in results if row.scenario == scenario}
        )
        for run_index in run_indices:
            by_variant = {
                row.variant: getattr(row, field)
                for row in results
                if row.scenario == scenario and row.run_index == run_index
            }
            legacy = by_variant.get("legacy")
            projected = by_variant.get("projected")
            if legacy is None or projected is None or float(legacy) <= 0:
                continue
            legacy_values.append(float(legacy))
            projected_values.append(float(projected))
        pair_counts[scenario] = len(legacy_values)
        if not legacy_values:
            continue
        ratio = statistics.median(projected_values) / statistics.median(legacy_values)
        ratios.append(ratio)
        scenarios[scenario] = ratio
    return {
        "scenario_count": len(ratios),
        "scenario_pair_counts": pair_counts,
        "p50_ratio": statistics.median(ratios) if ratios else None,
        "p95_ratio": percentile(ratios, 0.95),
        "max_ratio": max(ratios) if ratios else None,
        "scenario_ratios": scenarios,
    }


def score(helpers: Any, item: Scenario, variant: str, run_index: int, status_code: int, response: dict[str, Any], total_ms: int, first_ms: int | None, meaningful_ms: int | None, pricing: dict[str, Any] | None, error: str | None) -> Result:
    calls = helpers.parse_tool_calls(response)
    args = calls[0].arguments if len(calls) == 1 and calls[0].name == "submit_evaluation" else {}
    answer = str(args.get("answer") or "")
    observed_status = str(args.get("status") or "")
    evidence_present = args.get("evidence_present") if isinstance(args.get("evidence_present"), bool) else None
    exact_pass, status_pass, privacy_pass, evidence_present_pass = grade_fields(
        item, answer, observed_status, evidence_present
    )
    usage = response.get("usage") or {}
    input_tokens = usage.get("input_tokens")
    output_tokens = usage.get("output_tokens")
    cached_tokens = (usage.get("input_tokens_details") or {}).get("cached_tokens")
    cost = helpers.compute_cost(pricing, input_tokens, cached_tokens, output_tokens)
    passed = status_code == 200 and exact_pass and status_pass and privacy_pass and evidence_present_pass and error is None
    return Result(
        scenario=item.name,
        surface=item.surface,
        variant=variant,
        run_index=run_index,
        status_code=status_code,
        total_ms=total_ms,
        first_output_ms=first_ms,
        first_meaningful_answer_ms=meaningful_ms,
        input_tokens=input_tokens,
        cached_tokens=cached_tokens,
        output_tokens=output_tokens,
        cost_usd=cost,
        answer=answer,
        observed_status=observed_status,
        evidence_present=evidence_present,
        evidence_present_pass=evidence_present_pass,
        exact_pass=exact_pass,
        status_pass=status_pass,
        privacy_pass=privacy_pass,
        passed=passed,
        error=error,
    )


def grade_fields(
    item: Scenario,
    answer: str,
    observed_status: str,
    evidence_present: bool | None,
) -> tuple[bool, bool, bool, bool]:
    answer_norm = normalized(answer)
    exact_pass = any(
        contains_expected_answer(answer, expected)
        for expected in (item.expected_answer, *item.accepted_answers)
    )
    status_pass = normalized(item.expected_status) == normalized(observed_status)
    privacy_pass = not item.privacy_case or not any(
        secret in answer_norm for secret in ("14 september", "wife birthday", "must not disclose")
    )
    return exact_pass, status_pass, privacy_pass, evidence_present is True


def rescore_report(source: Path, output: Path | None = None) -> dict[str, Any]:
    """Re-gate stored parsed provider outputs without issuing new calls."""
    report = json.loads(source.read_text(encoding="utf-8"))
    fixtures = {item.name: item for item in scenarios()}
    rescored: list[Result] = []
    for stored in report.get("results", []):
        item = fixtures.get(str(stored.get("scenario")))
        if item is None:
            raise RuntimeError(f"stored report contains unknown scenario {stored.get('scenario')}")
        row = Result(**stored)
        (
            row.exact_pass,
            row.status_pass,
            row.privacy_pass,
            row.evidence_present_pass,
        ) = grade_fields(item, row.answer, row.observed_status, row.evidence_present)
        row.passed = (
            row.status_code == 200
            and row.exact_pass
            and row.status_pass
            and row.privacy_pass
            and row.evidence_present_pass
            and row.error is None
        )
        rescored.append(row)
    runtime_payload = report.get("runtime_fixtures") or {}
    runtime_cases = {
        str(case.get("name")): case
        for case in runtime_payload.get("cases", [])
        if isinstance(case, dict) and case.get("name")
    }
    summary = summarize(rescored, fixtures)
    failures = gate_failures(rescored, summary, runtime_cases or None)
    report["summary"] = summary
    report["gate_failures"] = failures
    report["results"] = [asdict(row) for row in rescored]
    report["rescored_at"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    report["provider_calls_reused"] = len(rescored)
    report.setdefault("generated_at", report["rescored_at"])
    report.setdefault("profile", {"name": "stored-live-report"})
    report.setdefault("runs", max((row.run_index for row in rescored), default=0))
    report.setdefault("coverage_limitations", [])
    limitation = focused_run_limitation(summary)
    if limitation and limitation not in report["coverage_limitations"]:
        report["coverage_limitations"].append(limitation)
    write_report(output or source.parent, report)
    return report


def summarize(results: list[Result], fixture_map: dict[str, Scenario]) -> dict[str, Any]:
    selected_scenarios = sorted({row.scenario for row in results})
    summary: dict[str, Any] = {
        "calls": len(results),
        "selected_scenarios": selected_scenarios,
        "full_performance_matrix": set(selected_scenarios)
        == {item.name for item in scenarios()},
        "variants": {},
        "surfaces": {},
    }
    for variant in ("legacy", "projected"):
        rows = [row for row in results if row.variant == variant]
        totals = [row.total_ms for row in rows]
        first_outputs = [row.first_output_ms for row in rows if row.first_output_ms is not None]
        meaningful = [row.first_meaningful_answer_ms for row in rows if row.first_meaningful_answer_ms is not None]
        input_tokens = sum(row.input_tokens or 0 for row in rows)
        cached_tokens = sum(row.cached_tokens or 0 for row in rows)
        summary["variants"][variant] = {
            "pass_rate": sum(row.passed for row in rows) / len(rows) if rows else 0.0,
            "exact_rate": sum(row.exact_pass for row in rows) / len(rows) if rows else 0.0,
            "privacy_rate": sum(row.privacy_pass for row in rows) / len(rows) if rows else 0.0,
            "evidence_presence_rate": sum(row.evidence_present_pass for row in rows) / len(rows) if rows else 0.0,
            "p50_ms": statistics.median(totals) if totals else None,
            "p95_ms": percentile(totals, 0.95),
            "first_output_p50_ms": statistics.median(first_outputs) if first_outputs else None,
            "first_output_p95_ms": percentile(first_outputs, 0.95),
            "first_meaningful_answer_p50_ms": statistics.median(meaningful) if meaningful else None,
            "first_meaningful_answer_p95_ms": percentile(meaningful, 0.95),
            "input_tokens": input_tokens,
            "cached_tokens": cached_tokens,
            "cache_read_pct": (cached_tokens / input_tokens * 100.0) if input_tokens else None,
            "cost_usd": sum(row.cost_usd or 0.0 for row in rows),
        }
    for surface in ("chat", "realtime_voice", "autonomous_task"):
        rows = [row for row in results if row.surface == surface and row.variant == "projected"]
        summary["surfaces"][surface] = {
            "pass_rate": sum(row.passed for row in rows) / len(rows) if rows else 0.0,
            "cases": len(rows),
        }
    large_names = {name for name, item in fixture_map.items() if item.large_result}
    legacy_tokens = sum((row.input_tokens or 0) for row in results if row.variant == "legacy" and row.scenario in large_names)
    projected_tokens = sum((row.input_tokens or 0) for row in results if row.variant == "projected" and row.scenario in large_names)
    summary["large_result_input_token_reduction_pct"] = ((legacy_tokens - projected_tokens) / legacy_tokens * 100.0) if legacy_tokens else None
    summary["large_result_cost_usd"] = {
        "legacy": sum((row.cost_usd or 0.0) for row in results if row.variant == "legacy" and row.scenario in large_names),
        "projected": sum((row.cost_usd or 0.0) for row in results if row.variant == "projected" and row.scenario in large_names),
    }
    summary["paired_latency"] = {
        "total_ms": paired_scenario_latency_summary(results, "total_ms"),
        "first_output_ms": paired_scenario_latency_summary(results, "first_output_ms"),
        "first_meaningful_answer_ms": paired_scenario_latency_summary(
            results, "first_meaningful_answer_ms"
        ),
    }
    return summary


def focused_run_limitation(summary: dict[str, Any]) -> str | None:
    if summary.get("full_performance_matrix") is True:
        return None
    return (
        "Focused scenario selection runs semantic correctness, privacy, and runtime-fixture "
        "gates only; aggregate token-reduction, cross-scenario latency, and provider-cost "
        "gates require the complete scenario matrix."
    )


def runtime_fixture_failures(runtime_cases: dict[str, dict[str, Any]]) -> list[str]:
    failures: list[str] = []
    expected_lifecycle = {
        "chat": ("chat", "chat_lifecycle"),
        "realtime_voice": ("ephemeral_voice", "ephemeral_voice"),
        "autonomous_task": ("task", "task_lifecycle"),
    }
    for name, case in runtime_cases.items():
        runtime = case.get("runtime") or {}
        kind = runtime.get("kind")
        if kind == "materialized_projection" and runtime.get("read_hash_verified") is not True:
            failures.append(f"{name}: canonical materialized read was not hash verified")
        if kind == "materialized_projection":
            expected_owner, expected_retention = expected_lifecycle[str(case.get("surface"))]
            if runtime.get("owner_kind") != expected_owner:
                failures.append(f"{name}: wrong production owner lifecycle for {case.get('surface')}")
            if runtime.get("retention_class") != expected_retention:
                failures.append(f"{name}: wrong production retention class for {case.get('surface')}")
        if name == "continuation_omitted_record":
            if runtime.get("continuation_pages_read") != 2:
                failures.append(f"{name}: production continuation did not read two pages")
            if runtime.get("continuation_page_projected") is not True:
                failures.append(
                    f"{name}: continuation page bypassed the production structure-aware projector"
                )
        if name == "reference_revoked" and runtime.get("revocation_denied") is not True:
            failures.append(f"{name}: current-authority read did not reject the stale revision")
        if kind == "staged_context":
            if runtime.get("deadline_reached") is not True:
                failures.append(f"{name}: staged fixture did not reach the configured absolute deadline")
            if runtime.get("accepted_contribution_count", 0) < 1:
                failures.append(f"{name}: completed staged sibling did not contribute canonical evidence")
            if runtime.get("turn_generation") != 1:
                failures.append(f"{name}: canonical turn generation was not preserved")
            if not runtime.get("relevance_query_digest") or not runtime.get("reuse_key_fingerprint"):
                failures.append(f"{name}: canonical query/reuse binding is missing")
            statuses = {
                str(row.get("stage", {}).get("name")): row
                for row in runtime.get("canonical_stage_statuses", [])
                if isinstance(row, dict) and isinstance(row.get("stage"), dict)
            }
            expected_completed = (
                "hybrid_memory" if name == "procedure_timeout_memory_survives" else "reusable_procedures"
            )
            expected_timeout = (
                "reusable_procedures" if name == "procedure_timeout_memory_survives" else "hybrid_memory"
            )
            if statuses.get(expected_completed, {}).get("state") != "completed":
                failures.append(f"{name}: completed sibling status was not retained")
            if statuses.get(expected_timeout, {}).get("state") != "timed_out":
                failures.append(f"{name}: pending sibling was not classified timed_out")
    return failures


def gate_failures(
    results: list[Result],
    summary: dict[str, Any],
    runtime_cases: dict[str, dict[str, Any]] | None = None,
) -> list[str]:
    failures: list[str] = []
    if runtime_cases is not None:
        failures.extend(runtime_fixture_failures(runtime_cases))
    projected = [row for row in results if row.variant == "projected"]
    failed = sorted({row.scenario for row in projected if not row.passed})
    if failed:
        failures.append("projected exact/status/privacy failures: " + ", ".join(failed))
    for surface, values in summary["surfaces"].items():
        if values["cases"] == 0:
            continue
        if values["pass_rate"] < 1.0:
            failures.append(f"{surface} projected pass rate is {values['pass_rate']:.1%}, expected 100%")
    reduction = summary.get("large_result_input_token_reduction_pct")
    if (
        summary.get("full_performance_matrix") is True
        and reduction is not None
        and reduction < 20.0
    ):
        failures.append(f"large-result input-token reduction is {reduction:.1f}%, expected >=20%")
    legacy = summary["variants"]["legacy"]
    projected_summary = summary["variants"]["projected"]
    if summary.get("full_performance_matrix") is True:
        for metric, values in summary.get("paired_latency", {}).items():
            incomplete = sorted(
                scenario
                for scenario, count in values.get("scenario_pair_counts", {}).items()
                if count < MIN_PERFORMANCE_PAIRS
            )
            if incomplete:
                failures.append(
                    f"{metric} has fewer than {MIN_PERFORMANCE_PAIRS} complete legacy/projected pairs: "
                    + ", ".join(incomplete)
                )
            ratio = values.get("p95_ratio")
            if ratio is not None and ratio > 1.15:
                failures.append(
                    f"projected paired scenario-median {metric} p95 regressed by more than 15% ({ratio:.3f}x)"
                )
    if summary.get("full_performance_matrix") is True:
        large_cost = summary.get("large_result_cost_usd") or {}
        legacy_cost = large_cost.get("legacy")
        projected_cost = large_cost.get("projected")
        if legacy_cost and projected_cost is not None and projected_cost >= legacy_cost:
            failures.append(
                f"projected provider cost did not improve ({projected_cost:.6f} vs {legacy_cost:.6f})"
            )
    return failures


def write_report(output: Path, report: dict[str, Any]) -> None:
    output.mkdir(parents=True, exist_ok=True)
    (output / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
    (output / "results.jsonl").write_text(
        "".join(json.dumps(row, sort_keys=True) + "\n" for row in report["results"]),
        encoding="utf-8",
    )
    rows = "".join(
        "<tr>" + "".join(f"<td>{html.escape(str(value))}</td>" for value in (row["scenario"], row["surface"], row["variant"], row["run_index"], "PASS" if row["passed"] else "FAIL", row["input_tokens"], row["total_ms"], row["first_output_ms"], row["first_meaningful_answer_ms"], row["answer"])) + "</tr>"
        for row in report["results"]
    )
    failures = "".join(f"<li>{html.escape(item)}</li>" for item in report["gate_failures"]) or "<li>None</li>"
    limitations = "".join(
        f"<li>{html.escape(item)}</li>" for item in report.get("coverage_limitations", [])
    ) or "<li>None</li>"
    runtime_summary = html.escape(json.dumps(report.get("runtime_fixtures", {"runtime_backed": False}), indent=2))
    document = f"""<!doctype html><html><head><meta charset='utf-8'><title>Tool result projection and staged context</title><style>body{{font:14px system-ui;margin:2rem;color:#202426;background:#fffdfa}}table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #d8d2c8;padding:.45rem;text-align:left;vertical-align:top}}th{{background:#f1ece5}}code,pre{{font-family:ui-monospace,monospace}}.summary{{display:grid;grid-template-columns:repeat(auto-fit,minmax(15rem,1fr));gap:.75rem}}.card{{border:1px solid #d8d2c8;border-radius:.7rem;padding:1rem;background:white}}</style></head><body><h1>Tool-result projection & staged-context live A/B</h1><p>{html.escape(report['generated_at'])} · profile {html.escape(report['profile']['name'])} · {report['runs']} randomized repeats</p><div class='summary'><div class='card'><h2>Projected</h2><pre>{html.escape(json.dumps(report['summary']['variants']['projected'], indent=2))}</pre></div><div class='card'><h2>Legacy</h2><pre>{html.escape(json.dumps(report['summary']['variants']['legacy'], indent=2))}</pre></div></div><h2>Gate failures</h2><ul>{failures}</ul><h2>Coverage limitations</h2><ul>{limitations}</ul><details><summary>Production runtime fixtures</summary><pre>{runtime_summary}</pre></details><h2>Calls</h2><table><thead><tr><th>Scenario</th><th>Surface</th><th>Variant</th><th>Run</th><th>Gate</th><th>Input tokens</th><th>Total ms</th><th>First output ms</th><th>Meaningful answer ms</th><th>Answer</th></tr></thead><tbody>{rows}</tbody></table></body></html>"""
    (output / "report.html").write_text(document, encoding="utf-8")


def fake_response(answer: str, status: str) -> dict[str, Any]:
    return {"output": [{"type": "function_call", "name": "submit_evaluation", "call_id": "call-self-test", "arguments": json.dumps({"answer": answer, "status": status, "evidence_present": True})}], "usage": {"input_tokens": 100, "output_tokens": 10, "input_tokens_details": {"cached_tokens": 0}}}


def self_test(root: Path, helpers: Any, output: Path | None = None) -> None:
    items = scenarios()
    assert len(items) == 17
    beyond = next(item for item in items if item.name == "owner_fact_beyond_prefix")
    assert "14 September" not in json.dumps(legacy_view(beyond))
    assert "14 September" in json.dumps(beyond.projected)
    results = [
        score(
            helpers,
            item,
            "projected",
            1,
            200,
            fake_response(item.expected_answer, item.expected_status),
            10,
            4,
            6,
            None,
            None,
        )
        for item in items
    ]
    assert all(item.passed for item in results)
    report = {"generated_at": "self-test", "profile": {"name": "self-test"}, "runs": 1, "summary": summarize(results, {item.name: item for item in items}), "gate_failures": [], "results": [asdict(item) for item in results]}
    import tempfile
    with tempfile.TemporaryDirectory(prefix="tool-projection-eval-") as temp:
        write_report(Path(temp), report)
        assert (Path(temp) / "report.json").is_file()
        assert (Path(temp) / "results.jsonl").is_file()
        assert (Path(temp) / "report.html").is_file()
    if output is not None:
        write_report(output, report)
        print(f"Provider-free JSON report: {output / 'report.json'}")
        print(f"Provider-free JSONL results: {output / 'results.jsonl'}")
        print(f"Provider-free HTML report: {output / 'report.html'}")
    print("tool-result projection/context live evaluator self-test passed")


def parse_args(root: Path, helpers: Any) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=helpers.default_config_path(root))
    parser.add_argument("--profile", default=TERRA_PROFILE)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--seed", type=int, default=7272026)
    parser.add_argument("--max-output-tokens", type=int, default=512)
    parser.add_argument("--timeout-secs", type=int)
    parser.add_argument("--pricing-file", type=Path)
    parser.add_argument("--env-file", type=Path, action="append")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--runtime-fixtures", type=Path)
    parser.add_argument("--rescore-report", type=Path)
    parser.add_argument("--scenario", action="append")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--no-gate", action="store_true")
    return parser.parse_args()


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    helpers = load_helpers(root)
    args = parse_args(root, helpers)
    if args.self_test:
        self_test(root, helpers, args.output_dir)
        return 0
    if args.rescore_report:
        try:
            report = rescore_report(args.rescore_report.expanduser(), args.output_dir)
        except Exception as error:
            print(f"stored report rescore error: {error}", file=sys.stderr)
            return 2
        for failure in report["gate_failures"]:
            print(f"GATE FAIL: {failure}", file=sys.stderr)
        destination = args.output_dir or args.rescore_report.expanduser().parent
        print(f"Rescored {report['provider_calls_reused']} stored live calls without provider spend")
        print(f"JSON report: {destination / 'report.json'}")
        print(f"JSONL results: {destination / 'results.jsonl'}")
        print(f"HTML report: {destination / 'report.html'}")
        return 0 if args.no_gate or not report["gate_failures"] else 1
    if args.runs < 1:
        print("--runs must be positive", file=sys.stderr)
        return 2
    items = [item for item in scenarios() if not args.scenario or item.name in args.scenario]
    if not items:
        print("no matching scenarios", file=sys.stderr)
        return 2
    try:
        profile = helpers.load_profile(args.config.expanduser(), args.profile)
    except Exception as error:
        print(f"profile configuration error: {error}", file=sys.stderr)
        return 2
    if profile.provider.lower() != "openai":
        print(f"live eval requires an OpenAI Responses profile; got {profile.provider}", file=sys.stderr)
        return 2
    jobs = build_jobs(items, args.runs, args.seed)
    if args.dry_run:
        print(json.dumps({"profile": profile.name, "model": profile.model, "calls": len(jobs), "runs": args.runs, "seed": args.seed, "scenarios": [item.name for item in items], "runtime_fixture_generation": "required_before_live_calls", "sample_payload_bytes": len(json.dumps(payload(profile, items[0], "projected", args.max_output_tokens)).encode())}, indent=2))
        return 0
    for env_file in args.env_file or [Path.home() / "MagicianNotes/.env.development", Path.home() / "MagicianNotes/.env", root / ".env.development", root / ".env"]:
        if env_file.is_file():
            helpers.load_dotenv(env_file)
    api_key = os.environ.get(profile.api_key_env)
    if not api_key:
        print(f"{profile.api_key_env} is not set", file=sys.stderr)
        return 2
    try:
        if args.runtime_fixtures:
            runtime_report = json.loads(args.runtime_fixtures.read_text(encoding="utf-8"))
            runtime_cases = load_runtime_fixtures(args.runtime_fixtures, items)
        else:
            runtime_cases, runtime_report = generate_runtime_fixtures(
                root,
                args.config.expanduser(),
                items,
            )
    except Exception as error:
        print(f"runtime fixture generation error: {error}", file=sys.stderr)
        return 2
    pricing_path = args.pricing_file or Path.home() / "MagicianNotes/llm_pricing.json"
    pricing = helpers.select_pricing_row(pricing_path, profile.provider, profile.model) if pricing_path.is_file() else None
    endpoint = helpers.responses_url(profile)
    timeout = args.timeout_secs or profile.timeout_secs
    results: list[Result] = []
    for index, (run_index, item, variant) in enumerate(jobs, 1):
        body = payload(profile, item, variant, min(profile.configured_max_output_tokens, args.max_output_tokens), runtime_cases)
        status_code, response, total_ms, first_ms, tool_ms, error = helpers.run_live_request(api_key, endpoint, body, timeout)
        result = score(helpers, item, variant, run_index, status_code, response or {}, total_ms, first_ms, tool_ms, pricing, error)
        results.append(result)
        print(f"[{index}/{len(jobs)}] {variant}/{item.name}/run-{run_index}: {'PASS' if result.passed else 'FAIL'} {total_ms}ms", flush=True)
    fixture_map = {item.name: item for item in items}
    summary = summarize(results, fixture_map)
    failures = gate_failures(results, summary, runtime_cases)
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output = args.output_dir or root / "coverage/evals/tool-result-projection-context" / f"live-{stamp}"
    report = {
        "generated_at": stamp,
        "profile": asdict(profile),
        "runs": args.runs,
        "scenario_count": len(items),
        "randomized_variant_order": True,
        "counterbalanced_variant_order": True,
        "random_seed": args.seed,
        "runtime_fixtures": runtime_report,
        "coverage_limitations": [
            "The realtime_rotation_replay and multi_tool_reuse cases grade provider consumption of production-projected protocol evidence; this lane does not force an actual transport rotation or provider reconnect.",
            "Actual OpenAI rotation replay and provider protocol balance remain covered by deterministic adapter/orchestrator tests.",
        ]
        + ([focused_run_limitation(summary)] if focused_run_limitation(summary) else []),
        "summary": summary,
        "gate_failures": failures,
        "results": [asdict(item) for item in results],
    }
    write_report(output, report)
    print(f"JSON report: {output / 'report.json'}")
    print(f"JSONL results: {output / 'results.jsonl'}")
    print(f"HTML report: {output / 'report.html'}")
    if failures:
        for failure in failures:
            print(f"GATE FAIL: {failure}", file=sys.stderr)
        return 0 if args.no_gate else 1
    print("Tool-result projection/context live gate: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Plan or live-evaluate the six Ollama logical-chunk adapters.

The live mode calls configured local Ollama candidates and, unless
``--local-only`` is set, their current cloud comparison profiles. It uses
synthetic committed episodes, receives no persistence handle, and saves only
hashes, schema/golden verdicts, timings, and logical-chunk telemetry.
Archive runs additionally fail unless runtime-owned episode membership is
covered exactly once and every adapter group stays within the six-episode
durable checkpoint bound.
"""

from __future__ import annotations

import argparse
import copy
import html
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import yaml
import pathlib

# The router's profiles and operation_mapping live in a sibling
# `llm-router.yaml`; reading the config file alone yields neither.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from magician_config_text import read_config_text  # noqa: E402



REPO_ROOT = Path(__file__).resolve().parents[1]
SOURCE_FIXTURES = (
    REPO_ROOT
    / "data/magician_v2/llm_chunking_evals/fixtures/consolidation-sources-v1.json"
)
BASELINE = (
    REPO_ROOT
    / "data/magician_v2/llm_chunking_evals/phase0-telemetry-baseline-v1.json"
)
GENERATION_CASES = (
    REPO_ROOT
    / "data/magician_v2/llm_chunking_evals/consolidation-generation-cases-v1.json"
)
REPO_CONFIG = REPO_ROOT / "magician-config.yaml"
LIVE_CONFIG = Path.home() / "MagicianNotes/magician-config.yaml"
ACTIVATION_MANIFEST = (
    REPO_ROOT
    / "data/magician_v2/llm_chunking_evals/activation-manifest-v1.yaml"
)
PRODUCTION_MODEL = "qwen3.8-ud2-mtp"
# The qwen-27b-bonsai candidate was retired; evaluate the production model
# itself by default (it also loads in stock Ollama, unlike the bonsai GGUF).
DEFAULT_EVAL_MODEL = "qwen3.8-ud2-mtp"

SPECS = (
    (
        "memory_temperature_utility_review",
        "memory_utility_review_v1",
        "op-memory-utility-review-local-chunked",
        ("memories",),
    ),
    (
        "memory_episode_quality_classification",
        "memory_episode_quality_v1",
        "op-memory-episode-quality-local-chunked",
        ("episode_signals",),
    ),
    (
        "memory_entity_extraction",
        "memory_entities_v1",
        "op-memory-entity-extraction-local-chunked",
        ("entities",),
    ),
    (
        "distill_evidence",
        "evidence_distill_v1",
        "op-memory-evidence-distillation-local-chunked",
        ("proposals",),
    ),
    (
        "memory_environment_knowledge_extraction",
        "memory_environment_v1",
        "op-memory-environment-extraction-local-chunked",
        ("environments",),
    ),
    (
        "memory_archive_summary",
        "memory_archive_v1",
        "op-memory-archive-summary-local-chunked",
        ("archive_entries", "total_episodes_archived"),
    ),
)
BASELINE_PROFILES = {
    "memory_temperature_utility_review": "op-memory-episode-quality-standard",
    "memory_episode_quality_classification": "op-memory-episode-quality-standard",
    "memory_entity_extraction": "op-memory-entity-extraction-fast",
    "distill_evidence": "op-memory-evidence-distillation-fast",
    "memory_environment_knowledge_extraction": "op-memory-environment-extraction-fast",
    "memory_archive_summary": "op-memory-archive-summary-fast",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--config", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument(
        "--local-model",
        default=os.environ.get("OLLAMA_CHUNK_EVAL_MODEL", DEFAULT_EVAL_MODEL),
        help=(
            "Ollama model tag to inject into an isolated evaluation config "
            f"(default: {DEFAULT_EVAL_MODEL})"
        ),
    )
    parser.add_argument(
        "--served-model-label",
        help=(
            "actual backend model identity for reports when a compatibility "
            "bridge serves a different model behind --local-model"
        ),
    )
    parser.add_argument(
        "--runtime-kind",
        choices=("ollama", "llama-server", "mlx-lm", "openai-chat"),
        default="ollama",
        help="actual local inference runtime behind the evaluation endpoint",
    )
    parser.add_argument(
        "--structured-output-mode",
        choices=(
            "backend_grammar_constrained",
            "prompt_constrained",
            "not_requested",
        ),
        help=(
            "override how the runtime enforces structured output; inferred from "
            "--runtime-kind when omitted"
        ),
    )
    parser.add_argument(
        "--phase-timing-source",
        choices=("backend_native", "observed_openai_stream", "unavailable"),
        help=(
            "override the companion bridge phase-timing capability; inferred "
            "from --runtime-kind when omitted"
        ),
    )
    parser.add_argument(
        "--runtime-context-tokens",
        type=int,
        help=(
            "actual server/model context limit for report comparability; MLX-LM "
            "does not accept Ollama's per-request num_ctx control"
        ),
    )
    parser.add_argument(
        "--operation",
        action="append",
        choices=[spec[0] for spec in SPECS],
        help="evaluate only this operation; repeat the flag to select several",
    )
    parser.add_argument(
        "--compact-smoke",
        action="store_true",
        help=(
            "use one copy of each committed episode template without synthetic "
            "padding; preserves real adapter prompts/golden checks for quick "
            "model-to-model smoke comparisons"
        ),
    )
    parser.add_argument(
        "--merge-report",
        action="append",
        type=Path,
        help=(
            "compose existing report.json files in order; a later report "
            "supersedes only operations it contains"
        ),
    )
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--local-only",
        action="store_true",
        help="skip the explicit current-cloud comparison in live mode",
    )
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args()


def build_fixture_suite(
    operations: set[str] | None = None, compact_smoke: bool = False
) -> dict[str, Any]:
    source = json.loads(SOURCE_FIXTURES.read_text(encoding="utf-8"))
    generation = json.loads(GENERATION_CASES.read_text(encoding="utf-8"))
    records = []
    padding = source["padding"]["sentence"]
    cycle_count = 1 if compact_smoke else 6
    for cycle in range(cycle_count):
        for template in source["episode_templates"]:
            record = copy.deepcopy(template["record"])
            prefix = "ep-compact" if compact_smoke else f"ep-phase6-{cycle:02d}"
            record["episode_id"] = f"{prefix}-{template['template_id']}"
            record["trigger_seq"] = cycle * len(source["episode_templates"]) + len(records)
            if not compact_smoke:
                record["observations"] = list(record.get("observations", [])) + [
                    padding.format(
                        sequence=f"{cycle}-{index}", source_id=record["episode_id"]
                    )
                    * 5
                    for index in range(8)
                ]
            records.append(record)
    utility = source["utility_review_templates"][0]["input"]
    forbidden = generation["global_assertions"]["forbidden_output_substrings"]
    required_by_adapter = {
        "memory_utility_review_v1": [
            "mem:workflow:fixture-service-start",
            "mem:stale:fixture-direct-launch",
            "mem:irrelevant:fixture-color",
        ],
        "memory_episode_quality_v1": [record["episode_id"] for record in records],
        "memory_entities_v1": ["Project Atlas", "Priya Nair"],
        "memory_environment_v1": [
            "make run-fixture-service",
            "direct binary launch",
            "fixture://local-service/health",
        ],
        "evidence_distill_v1": [record["episode_id"] for record in records],
        "memory_archive_v1": ["Project Atlas", "make run-fixture-service"],
    }
    cases = []
    for operation, adapter_id, _profile, keys in SPECS:
        if operations is not None and operation not in operations:
            continue
        fixture_input: Any = utility if adapter_id == "memory_utility_review_v1" else {
            "episodes": records
        }
        cases.append(
            {
                "name": (
                    f"compact_smoke_{operation}"
                    if compact_smoke
                    else f"synthetic_{operation}"
                ),
                "operation": operation,
                "adapter_id": adapter_id,
                "input": fixture_input,
                "required_top_level_keys": list(keys),
                "required_output_substrings": required_by_adapter[adapter_id],
                "forbidden_output_substrings": forbidden,
                "required_source_episode_ids": (
                    [record["episode_id"] for record in records]
                    if adapter_id == "memory_archive_v1"
                    else []
                ),
                "maximum_archive_group_episodes": (
                    6 if adapter_id == "memory_archive_v1" else None
                ),
                "minimum_chunk_count": (
                    1
                    if compact_smoke or adapter_id == "memory_utility_review_v1"
                    else 2
                ),
            }
        )
    return {
        "baseline_reference": str(BASELINE.relative_to(REPO_ROOT)),
        "cases": cases,
    }


def router(config_path: Path) -> dict[str, Any]:
    payload = yaml.safe_load(read_config_text(config_path))
    return payload["llm"]["router"]


def materialize_eval_config(
    source: Path, output_dir: Path, local_model: str
) -> tuple[Path, list[Path]]:
    payload = yaml.safe_load(read_config_text(source))
    profiles = payload["llm"]["router"]["profiles"]
    for _operation, _adapter, profile_name, _keys in SPECS:
        profile = profiles.get(profile_name)
        if not profile:
            raise SystemExit(f"{source}: missing candidate profile {profile_name}")
        profile["model"] = local_model

    # A candidate model name can temporarily add a third unique local model to
    # the config (candidate + production generation + embedding). The focused
    # evaluator starts no runtime prewarm loop, but normal config loading still
    # enforces this capacity invariant. Raise it only in the isolated eval copy.
    ollama_runtime = payload.setdefault("runtime", {}).setdefault("ollama", {})
    ollama_runtime["max_loaded_models"] = max(
        int(ollama_runtime.get("max_loaded_models", 0)), 3
    )

    runtime_dir = output_dir / "eval-runtime"
    runtime_dir.mkdir(parents=True, exist_ok=True)
    eval_config = runtime_dir / "magician-config.yaml"
    eval_config.write_text(
        yaml.safe_dump(payload, sort_keys=False, allow_unicode=True),
        encoding="utf-8",
    )

    transient_links: list[Path] = []
    for filename in (".env", ".env.development"):
        source_env = source.parent / filename
        target_env = runtime_dir / filename
        if target_env.is_symlink() or target_env.exists():
            target_env.unlink()
        if source_env.exists():
            target_env.symlink_to(source_env)
            transient_links.append(target_env)
    return eval_config, transient_links


def validate_config_surfaces(primary: Path, local_model: str) -> dict[str, Any]:
    surfaces = [
        (REPO_CONFIG, PRODUCTION_MODEL),
        (LIVE_CONFIG, PRODUCTION_MODEL),
        (primary, local_model),
    ]
    errors: list[str] = []
    checked = []
    for surface, expected_model in surfaces:
        if not surface.exists():
            errors.append(f"missing config surface: {surface}")
            continue
        llm = router(surface)
        profiles = llm["profiles"]
        for operation, adapter, profile_name, _keys in SPECS:
            profile = profiles.get(profile_name)
            if not profile:
                errors.append(f"{surface}: missing {profile_name}")
                continue
            policy = profile.get("chunking", {})
            expected = {
                "provider": "ollama",
                "model": expected_model,
                "api_base_url": "http://localhost:11434/api/generate",
                "context_window_tokens": 32768,
                "max_output_tokens": 4096,
                "timeout_secs": 300,
            }
            for key, value in expected.items():
                if profile.get(key) != value:
                    errors.append(
                        f"{surface}: {profile_name}.{key}={profile.get(key)!r}, expected {value!r}"
                    )
            policy_expected = {
                "enabled": True,
                "adapter": adapter,
                "logical_window_tokens": 262144,
                "target_payload_tokens": 24576,
                "safety_margin_tokens": 2048,
                "fallback_policy": "same_provider_only",
            }
            for key, value in policy_expected.items():
                if policy.get(key) != value:
                    errors.append(
                        f"{surface}: {profile_name}.chunking.{key}={policy.get(key)!r}, expected {value!r}"
                    )
            metadata = profile.get("metadata", {})
            metadata_expected = {
                "format": "json",
                "num_ctx": 32768,
                "draft_num_predict": 4,
                "tool_choice_type": "none",
            }
            metadata_actual = {
                "format": metadata.get("format"),
                "num_ctx": metadata.get("options", {}).get("num_ctx"),
                "draft_num_predict": metadata.get("options", {}).get("draft_num_predict"),
                "tool_choice_type": metadata.get("tool_choice", {}).get("type"),
            }
            if metadata_actual != metadata_expected:
                errors.append(
                    f"{surface}: {profile_name}.metadata={metadata_actual!r}, expected {metadata_expected!r}"
                )
            current_mapping = llm["operation_mapping"].get(operation)
            if current_mapping != profile_name:
                errors.append(
                    f"{surface}: {operation} maps to {current_mapping!r}, expected activated candidate {profile_name!r}"
                )
        checked.append(str(surface))
    manifest = yaml.safe_load(ACTIVATION_MANIFEST.read_text(encoding="utf-8"))
    manifest_operations = {
        entry["operation"]: entry for entry in manifest.get("operations", [])
    }
    for operation, _adapter, candidate, _keys in SPECS:
        entry = manifest_operations.get(operation)
        if not entry:
            errors.append(f"activation manifest: missing {operation}")
            continue
        baseline = BASELINE_PROFILES[operation]
        if entry.get("candidate_profile") != candidate:
            errors.append(f"activation manifest: wrong candidate for {operation}")
        if entry.get("baseline_profile") != baseline or entry.get("rollback_profile") != baseline:
            errors.append(f"activation manifest: wrong baseline/rollback for {operation}")
        if entry.get("configured_profile") != candidate:
            errors.append(f"activation manifest: wrong configured profile for {operation}")
    if manifest.get("status") != "phase7_live_eval_passed_canary_pending":
        errors.append("activation manifest does not describe the verified Phase 7 shadow state")
    if manifest.get("production_behavior_changed_by_this_manifest") is not True:
        errors.append("activation manifest must record the configured behavior cutover")
    if manifest.get("approved") is not False:
        errors.append("activation manifest must remain unapproved until live gates pass")
    if manifest.get("canary", {}).get("scopes"):
        errors.append("activation manifest must not claim unverified canary scopes")
    expected_target = {
        "provider": "ollama",
        "model": PRODUCTION_MODEL,
        "api_base_url": "http://localhost:11434/api/generate",
        "physical_context_tokens": 32768,
        "logical_context_tokens": 262144,
        "target_payload_tokens": 24576,
        "static_overhead_reserve_tokens": 2048,
        "output_reserve_tokens": 4096,
        "safety_margin_tokens": 2048,
        "fallback_policy": "same_provider_only",
        "reasoning": "off",
        "structured_output": "json",
    }
    if manifest.get("target") != expected_target:
        errors.append(
            f"activation manifest target={manifest.get('target')!r}, expected {expected_target!r}"
        )
    checked.append(str(ACTIVATION_MANIFEST))
    return {"ok": not errors, "checked": checked, "errors": errors}


def run_rust_eval(
    fixture_path: Path,
    output_path: Path,
    execute: bool,
    compare_cloud: bool,
    runs: int,
    config_path: Path,
) -> dict[str, Any]:
    env = os.environ.copy()
    env["MAGICIAN_ROOT_DIR"] = str(config_path.resolve().parent)
    env.setdefault("CARGO_TARGET_DIR", "/Volumes/build/magician/builds")
    eval_binary = os.environ.get("MAGICIAN_CHUNK_EVAL_BINARY")
    command = (
        [eval_binary, "logical-chunk-eval"]
        if eval_binary
        else [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "magician",
            "--bin",
            "magician",
            "--",
            "logical-chunk-eval",
        ]
    ) + [
        "--fixtures",
        str(fixture_path),
        "--repeats",
        str(runs),
        "--output",
        str(output_path),
    ]
    if execute:
        command.append("--execute")
    if compare_cloud:
        command.append("--compare-cloud")
    completed = subprocess.run(
        command,
        cwd=REPO_ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=None,
        check=False,
    )
    if not output_path.exists():
        raise RuntimeError(
            f"logical-chunk-eval exited {completed.returncode} without a JSON report"
        )
    report = json.loads(output_path.read_text(encoding="utf-8"))
    report["process_exit_code"] = completed.returncode
    return report


def baseline_by_operation() -> dict[str, Any]:
    payload = json.loads(BASELINE.read_text(encoding="utf-8"))
    return {entry["operation"]: entry for entry in payload.get("operations", [])}


def enrich(
    report: dict[str, Any],
    config_validation: dict[str, Any],
    local_model: str,
    local_runtime: dict[str, Any],
) -> dict[str, Any]:
    baselines = baseline_by_operation()
    avoided = 0.0
    estimates = 0
    online_provider_calls = 0
    for case in report.get("cases", []):
        baseline = baselines.get(case["operation"], {})
        case["phase0_cloud_baseline"] = baseline
        calls = int(baseline.get("calls") or 0)
        cost = baseline.get("estimated_cost_usd")
        per_call = float(cost) / calls if cost is not None and calls else None
        logical_runs = len(case.get("runs", []))
        case["estimated_cloud_cost_avoided_usd"] = (
            round(per_call * logical_runs, 6) if per_call is not None else None
        )
        if per_call is not None:
            avoided += per_call * logical_runs
            estimates += logical_runs
        online_provider_calls += sum(
            int((run.get("logical_chunking") or {}).get("physical_call_count") or 0)
            for run in case.get("cloud_runs", [])
        )
    report["phase6"] = {
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "local_model": local_model,
        "local_runtime": local_runtime,
        "config_surfaces": config_validation,
        "estimated_cloud_cost_avoided_usd": round(avoided, 6) if estimates else None,
        "cost_method": "phase0 historical mean cost per operation multiplied by local shadow runs; estimate only",
        "durable_writes": 0,
        "online_provider_calls": online_provider_calls,
    }
    return report


def runtime_metadata(args: argparse.Namespace) -> dict[str, Any]:
    prompt_constrained = args.runtime_kind in ("mlx-lm", "openai-chat")
    if args.runtime_kind == "ollama":
        context_control = "per_request_num_ctx"
        model_lifecycle = "ollama_keep_alive"
        reasoning_control = "native_think_flag"
    elif args.runtime_kind == "llama-server":
        context_control = "server_startup_context"
        model_lifecycle = "server_process_owned"
        reasoning_control = "prompt_only"
    else:
        context_control = "model_or_server_startup_context"
        model_lifecycle = "server_process_owned"
        reasoning_control = "chat_template_enable_thinking_best_effort"
    metadata = {
        "kind": args.runtime_kind,
        "structured_output_mode": args.structured_output_mode
        or (
            "prompt_constrained"
            if prompt_constrained
            else "backend_grammar_constrained"
        ),
        "quality_latency_source": "evaluator_end_to_end_wall_clock",
        "timing_source": args.phase_timing_source
        or ("observed_openai_stream" if prompt_constrained else "backend_native"),
        "context_control": context_control,
        "model_lifecycle": model_lifecycle,
        "reasoning_control": reasoning_control,
    }
    if args.runtime_context_tokens is not None:
        metadata["context_tokens"] = args.runtime_context_tokens
    return metadata


def rows_jsonl(report: dict[str, Any]) -> str:
    rows = []
    for case in report.get("cases", []):
        for run in case.get("runs", []):
            rows.append(
                json.dumps(
                    {
                        "lane": "local_shadow",
                        "operation": case["operation"],
                        "adapter_id": case["adapter_id"],
                        "candidate_profile": case["candidate_profile"],
                        **run,
                    },
                    sort_keys=True,
                )
            )
        for run in case.get("cloud_runs", []):
            rows.append(
                json.dumps(
                    {
                        "lane": "cloud_comparison",
                        "operation": case["operation"],
                        "adapter_id": case["adapter_id"],
                        "baseline_profile": case["baseline_profile"],
                        **run,
                    },
                    sort_keys=True,
                )
            )
    return "\n".join(rows) + ("\n" if rows else "")


def render_html(report: dict[str, Any]) -> str:
    mode = report.get("mode", "unknown")
    readiness = report.get("readiness", {})
    cards = []
    for case in report.get("cases", []):
        plan = case.get("plan") or {}
        local_runs = case.get("runs", [])
        physical_calls = sum(
            int((run.get("logical_chunking") or {}).get("physical_call_count") or 0)
            for run in local_runs
        )
        local_repairs = sum(
            int((run.get("logical_chunking") or {}).get("local_repairs") or 0)
            for run in local_runs
        )
        validation_errors = sum(
            len(run.get("validation_errors") or []) for run in local_runs
        )
        cloud_runs = case.get("cloud_runs", [])
        cloud_calls = sum(
            int((run.get("logical_chunking") or {}).get("physical_call_count") or 0)
            for run in cloud_runs
        )
        cloud_repairs = sum(
            int((run.get("logical_chunking") or {}).get("local_repairs") or 0)
            for run in cloud_runs
        )
        cloud_validation_errors = sum(
            len(run.get("validation_errors") or []) for run in cloud_runs
        )
        if mode == "plan_only":
            status = "PLANNED" if case.get("plan") else "FAIL"
        else:
            status = "PASS" if case.get("plan") and all(
                run.get("golden_valid") for run in local_runs
            ) else "FAIL"
        cards.append(
            "<tr>"
            f"<td>{html.escape(case['operation'])}</td>"
            f"<td>{html.escape(case['adapter_id'])}</td>"
            f"<td>{status}</td>"
            f"<td>{plan.get('chunk_count', '—')}</td>"
            f"<td>{len(local_runs)}</td>"
            f"<td>{physical_calls or '—'}</td>"
            f"<td>{local_repairs}</td>"
            f"<td>{cloud_calls or '—'}</td>"
            f"<td>{cloud_repairs}</td>"
            f"<td>{validation_errors} / {cloud_validation_errors}</td>"
            f"<td>{fmt_rate(case.get('schema_validity_rate'))}</td>"
            f"<td>{fmt_rate(case.get('golden_validity_rate'))}</td>"
            f"<td>{case.get('latency_p50_ms') or '—'}</td>"
            f"<td>{case.get('latency_p90_ms') or '—'}</td>"
            f"<td>{fmt_rate(case.get('cloud_golden_validity_rate'))}</td>"
            f"<td>{case.get('cloud_latency_p90_ms') or '—'}</td>"
            f"<td>{'PASS' if case.get('slo_pass') else ('—' if case.get('slo_pass') is None else 'FAIL')}</td>"
            f"<td>{fmt_money(case.get('estimated_cloud_cost_avoided_usd'))}</td>"
            "</tr>"
        )
    config = report.get("phase6", {}).get("config_surfaces", {})
    runtime = report.get("phase6", {}).get("local_runtime") or {}
    qualification = report.get("qualification") or {}
    source_links = []
    for source in qualification.get("source_reports", []):
        source_path = Path(source)
        report_href = (
            source_path.parent / "report.html"
            if source_path.name == "report.json"
            else source_path
        )
        source_links.append(
            f'<a href="{html.escape(report_href.as_uri())}">'
            f"{html.escape(source_path.parent.name or source_path.name)}</a>"
        )
    qualification_html = ""
    if source_links:
        superseded = ", ".join(qualification.get("superseded_operations", []))
        qualification_html = (
            "<p>Composite qualification: latest case per operation from "
            + " · ".join(source_links)
            + (f". Superseded operations: <code>{html.escape(superseded)}</code>." if superseded else ".")
            + "</p>"
        )
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><title>Ollama logical chunking eval</title>
<style>
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;margin:32px;background:#0b1020;color:#e8edf7}}
.summary{{display:flex;gap:14px;flex-wrap:wrap;margin:20px 0}} .card{{background:#151d32;padding:14px 18px;border-radius:10px}}
table{{width:100%;border-collapse:collapse;background:#151d32}} th,td{{padding:10px;border-bottom:1px solid #2a3552;text-align:left}}
th{{color:#9eb1d7}} code{{color:#b7dcff}} a{{color:#79b8ff}}
</style></head><body>
<h1>Local logical-context chunking</h1>
<p>Phase 7 {html.escape(mode)} verification report. Synthetic fixtures; no durable writes.</p>
<div class="summary">
<div class="card">Readiness<br><strong>{html.escape(str(readiness.get('status', 'unknown')))}</strong></div>
<div class="card">Local model<br><strong>{html.escape(str(report.get('phase6', {}).get('local_model', '—')))}</strong></div>
<div class="card">Runtime<br><strong>{html.escape(str(runtime.get('kind', '—')))}</strong><br>{html.escape(str(runtime.get('structured_output_mode', '—')))} · context {html.escape(str(runtime.get('context_tokens', runtime.get('context_control', '—'))))}</div>
<div class="card">Durable writes<br><strong>{report.get('phase6', {}).get('durable_writes', '—')}</strong></div>
<div class="card">Config surfaces<br><strong>{'PASS' if config.get('ok') else 'FAIL'}</strong></div>
<div class="card">Estimated cloud cost avoided<br><strong>{fmt_money(report.get('phase6', {}).get('estimated_cloud_cost_avoided_usd'))}</strong></div>
</div>
{qualification_html}
<table><thead><tr><th>Operation</th><th>Adapter</th><th>Gate</th><th>Chunks</th><th>Runs</th><th>Local calls</th><th>Local repairs</th><th>Cloud calls</th><th>Cloud repairs</th><th>Validation errors L / C</th><th>Schema</th><th>Golden</th><th>Local p50 ms</th><th>Local p90 ms</th><th>Cloud golden</th><th>Cloud p90 ms</th><th>SLO</th><th>Cost avoided</th></tr></thead>
<tbody>{''.join(cards)}</tbody></table>
<p><a href="report.json">Full JSON</a> · <a href="results.jsonl">Per-run JSONL</a> · <a href="fixture-suite.json">Synthetic fixture suite</a></p>
</body></html>"""


def fmt_rate(value: Any) -> str:
    return "—" if value is None else f"{float(value) * 100:.1f}%"


def fmt_money(value: Any) -> str:
    return "—" if value is None else f"${float(value):.6f}"


def merge_reports(report_paths: list[Path], output_dir: Path) -> int:
    if len(report_paths) < 2:
        raise SystemExit("--merge-report must be supplied at least twice")
    loaded: list[tuple[Path, dict[str, Any]]] = []
    selected: dict[str, dict[str, Any]] = {}
    superseded: set[str] = set()
    for supplied in report_paths:
        path = supplied.resolve()
        if path.is_dir():
            path = path / "report.json"
        report = json.loads(path.read_text(encoding="utf-8"))
        if report.get("schema_version") != "ollama_logical_chunk_shadow_eval.v1":
            raise SystemExit(f"unsupported logical-chunk report schema: {path}")
        loaded.append((path, report))
        for case in report.get("cases", []):
            operation = case.get("operation")
            if operation in selected and selected[operation].get("runs"):
                superseded.add(operation)
            selected[operation] = copy.deepcopy(case)

    ordered_operations = [spec[0] for spec in SPECS]
    missing = [operation for operation in ordered_operations if operation not in selected]
    if missing:
        raise SystemExit(f"merged reports are missing operations: {', '.join(missing)}")
    cases = [selected[operation] for operation in ordered_operations]
    repeats = max((len(case.get("runs", [])) for case in cases), default=0)
    cloud_comparison = all(bool(case.get("cloud_runs")) for case in cases)

    def run_valid(run: dict[str, Any]) -> bool:
        return bool(run.get("success") and run.get("schema_valid") and run.get("golden_valid"))

    all_plans_valid = all(case.get("plan") is not None for case in cases)
    all_live_runs_valid = all(
        len(case.get("runs", [])) == repeats
        and all(run_valid(run) for run in case.get("runs", []))
        and (
            not cloud_comparison
            or (
                len(case.get("cloud_runs", [])) == repeats
                and all(run_valid(run) for run in case.get("cloud_runs", []))
            )
        )
        and case.get("slo_pass") is True
        for case in cases
    )
    checked = sorted(
        {
            checked_path
            for _path, report in loaded
            for checked_path in report.get("phase6", {})
            .get("config_surfaces", {})
            .get("checked", [])
        }
    )
    config_errors = [
        error
        for _path, report in loaded
        for error in report.get("phase6", {})
        .get("config_surfaces", {})
        .get("errors", [])
    ]
    local_models = sorted(
        {
            str(report.get("phase6", {}).get("local_model"))
            for _path, report in loaded
            if report.get("phase6", {}).get("local_model")
        }
    )
    runtime_variants = []
    runtime_keys: set[str] = set()
    for _path, source_report in loaded:
        runtime = source_report.get("phase6", {}).get("local_runtime")
        if not isinstance(runtime, dict) or not runtime:
            continue
        key = json.dumps(runtime, sort_keys=True)
        if key not in runtime_keys:
            runtime_keys.add(key)
            runtime_variants.append(copy.deepcopy(runtime))
    if len(runtime_variants) == 1:
        merged_runtime: dict[str, Any] | None = runtime_variants[0]
    elif runtime_variants:
        merged_runtime = {
            "kind": "mixed",
            "structured_output_mode": ", ".join(
                sorted(
                    {
                        str(runtime.get("structured_output_mode") or "unspecified")
                        for runtime in runtime_variants
                    }
                )
            ),
            "timing_source": ", ".join(
                sorted(
                    {
                        str(runtime.get("timing_source") or "unspecified")
                        for runtime in runtime_variants
                    }
                )
            ),
            "variants": runtime_variants,
        }
    else:
        merged_runtime = None
    online_provider_calls = sum(
        int((run.get("logical_chunking") or {}).get("physical_call_count") or 0)
        for case in cases
        for run in case.get("cloud_runs", [])
    )
    durable_writes = sum(
        int(run.get("durable_writes") or 0)
        for case in cases
        for lane in ("runs", "cloud_runs")
        for run in case.get(lane, [])
    )
    avoided_values = [
        float(case["estimated_cloud_cost_avoided_usd"])
        for case in cases
        if case.get("estimated_cloud_cost_avoided_usd") is not None
    ]
    merged = copy.deepcopy(loaded[0][1])
    merged.update(
        {
            "mode": "composite_local_shadow_with_cloud_comparison",
            "repeats": repeats,
            "cloud_comparison_executed": cloud_comparison,
            "cases": cases,
            "all_plans_valid": all_plans_valid,
            "all_live_runs_valid": all_live_runs_valid,
            "process_exit_code": 0 if all_plans_valid and all_live_runs_valid else 1,
            "phase6": {
                "generated_at": datetime.now(timezone.utc)
                .isoformat()
                .replace("+00:00", "Z"),
                "local_model": ", ".join(local_models) if local_models else None,
                "local_runtime": merged_runtime,
                "config_surfaces": {
                    "ok": not config_errors,
                    "checked": checked,
                    "errors": config_errors,
                },
                "estimated_cloud_cost_avoided_usd": (
                    round(sum(avoided_values), 6) if avoided_values else None
                ),
                "cost_method": (
                    "latest selected five-repeat case per operation; Phase 0 "
                    "historical mean cost estimate"
                ),
                "durable_writes": durable_writes,
                "online_provider_calls": online_provider_calls,
            },
            "qualification": {
                "strategy": "latest_case_per_operation",
                "source_reports": [str(path) for path, _report in loaded],
                "superseded_operations": sorted(superseded),
            },
        }
    )
    output_dir.mkdir(parents=True, exist_ok=True)
    fixture_source = loaded[0][0].parent / "fixture-suite.json"
    if fixture_source.exists():
        shutil.copyfile(fixture_source, output_dir / "fixture-suite.json")
    (output_dir / "report.json").write_text(
        json.dumps(merged, indent=2) + "\n", encoding="utf-8"
    )
    (output_dir / "results.jsonl").write_text(rows_jsonl(merged), encoding="utf-8")
    (output_dir / "report.html").write_text(render_html(merged), encoding="utf-8")
    print(f"Report: {(output_dir / 'report.html').as_uri()}")
    return 0 if all_plans_valid and all_live_runs_valid and not config_errors else 1


def self_test() -> int:
    suite = build_fixture_suite()
    assert len(suite["cases"]) == 6
    assert all(case["input"] for case in suite["cases"])
    archive_case = next(
        case for case in suite["cases"] if case["adapter_id"] == "memory_archive_v1"
    )
    assert archive_case["maximum_archive_group_episodes"] == 6
    assert len(archive_case["required_source_episode_ids"]) == len(
        archive_case["input"]["episodes"]
    )
    fake = {
        "mode": "plan_only",
        "readiness": {"status": "activated", "production_behavior_unchanged": False},
        "cases": [],
        "phase6": {"config_surfaces": {"ok": True}},
    }
    assert "Local logical-context chunking" in render_html(fake)
    fake_args = argparse.Namespace(
        runtime_kind="mlx-lm",
        structured_output_mode=None,
        phase_timing_source=None,
        runtime_context_tokens=32768,
    )
    assert runtime_metadata(fake_args) == {
        "kind": "mlx-lm",
        "structured_output_mode": "prompt_constrained",
        "quality_latency_source": "evaluator_end_to_end_wall_clock",
        "timing_source": "observed_openai_stream",
        "context_control": "model_or_server_startup_context",
        "model_lifecycle": "server_process_owned",
        "reasoning_control": "chat_template_enable_thinking_best_effort",
        "context_tokens": 32768,
    }
    print("ollama logical chunk evaluator self-test passed")
    return 0


def main() -> int:
    args = parse_args()
    if args.self_test:
        return self_test()
    if args.runs < 1:
        raise SystemExit("--runs must be a positive integer")
    if args.runtime_context_tokens is not None and args.runtime_context_tokens < 1:
        raise SystemExit("--runtime-context-tokens must be a positive integer")
    source_config_path = (args.config or LIVE_CONFIG).resolve()
    output_dir = (args.output_dir or REPO_ROOT / "coverage/evals/ollama-logical-chunking").resolve()
    if args.merge_report:
        if args.operation or args.dry_run or args.local_only:
            raise SystemExit(
                "--merge-report cannot be combined with --operation, --dry-run, or --local-only"
            )
        return merge_reports(args.merge_report, output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    fixture_path = output_dir / "fixture-suite.json"
    selected_operations = set(args.operation) if args.operation else None
    fixture_path.write_text(
        json.dumps(
            build_fixture_suite(selected_operations, compact_smoke=args.compact_smoke),
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    config_path, transient_links = materialize_eval_config(
        source_config_path, output_dir, args.local_model
    )
    validation = validate_config_surfaces(config_path, args.local_model)
    if not validation["ok"]:
        for link in transient_links:
            link.unlink(missing_ok=True)
        print(json.dumps(validation, indent=2), file=sys.stderr)
        return 1
    try:
        report = run_rust_eval(
            fixture_path,
            output_dir / "runner-report.json",
            execute=not args.dry_run,
            compare_cloud=not args.dry_run and not args.local_only,
            runs=args.runs,
            config_path=config_path,
        )
    finally:
        for link in transient_links:
            link.unlink(missing_ok=True)
    report = enrich(
        report,
        validation,
        args.served_model_label or args.local_model,
        runtime_metadata(args),
    )
    (output_dir / "report.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    (output_dir / "results.jsonl").write_text(rows_jsonl(report), encoding="utf-8")
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")
    print(f"Report: {(output_dir / 'report.html').as_uri()}")
    if report.get("process_exit_code") != 0:
        return int(report["process_exit_code"])
    if not report.get("all_plans_valid"):
        return 1
    if not args.dry_run and report.get("all_live_runs_valid") is not True:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

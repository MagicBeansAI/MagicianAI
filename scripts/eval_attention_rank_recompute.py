#!/usr/bin/env python3
"""Content-free contract evaluation for asynchronous attention rank recompute.

This evaluator is intentionally deterministic. It audits the frozen control-
plane invariants; it does not call a model, read live candidate content, or
promote runtime configuration.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def _load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("fixture root must be an object")
    return value


def _rate(checks: list[bool]) -> float:
    if not checks:
        return 0.0
    return sum(1 for check in checks if check) / len(checks)


def evaluate(document: dict[str, Any]) -> dict[str, Any]:
    outcome = document["outcome_and_job"]
    uniqueness = outcome["uniqueness"]
    survival_cases = outcome["accepted_outcome_survival"]
    worker = document["worker_lifecycle"]
    completion = document["completion_contract"]
    after = completion["after"]
    universe = document["current_universe"]
    cas = document["compare_and_set"]
    stale_cases = cas["stale_cases"]
    expected_stale = set(document["api_contract"]["stale_reason_codes"])

    uniqueness_checks = [
        uniqueness["jobs_after_first_accept"] == 1,
        uniqueness["jobs_after_idempotent_outcome_replay"] == 1,
        uniqueness["jobs_after_reconciliation_scan"] == 1,
    ]
    survival_checks = [case["outcome_count"] == 1 for case in survival_cases]
    survival_checks.extend(
        [
            survival_cases[0]["accepted_outcome_rolled_back"] is False,
            survival_cases[0]["job_count_before_reconciliation"] == 0,
            survival_cases[0]["job_count_after_reconciliation"] == 1,
            survival_cases[1]["lease_reclaimable"] is True,
            survival_cases[2]["outcome_rolled_back"] is False,
        ]
    )
    idempotency = worker["idempotent_completion"]
    idempotency_checks = [
        idempotency["completion_writes"] == 2,
        idempotency["terminal_rows"] == 1,
        idempotency["posterior_updates"] == 1,
        idempotency["result_documents"] == 1,
        worker["expired_lease"]["late_prior_owner_completion_accepted"] is False,
    ]
    binding_checks = [
        completion["status"] == "succeeded",
        completion["before"]["served_rank"]
        == outcome["outcome"]["served_rank_before"],
        after["semantics"] == "current_universe_diagnostic",
        after["affected_rank_delta"]
        == after["affected_rank_after"] - completion["before"]["served_rank"],
        after["current_source_revision"] == completion["before"]["source_revision"],
        after["universe_digest"] == universe["universe_digest"],
        after["recompute_generation"] == universe["source_generations"],
        isinstance(after["posterior_version"], int),
        isinstance(after["policy_snapshot_id"], str),
    ]
    stale_checks = [
        case["terminal_status"] == "stale"
        and case["after"] is None
        and case["posterior_updates"] == 0
        for case in stale_cases
    ]
    stale_checks.append({case["reason"] for case in stale_cases} == expected_stale)

    items = universe["items"]
    origin_sum = sum(universe["origin_totals"].values())
    evidence = universe["evidence"]
    reconciliation_checks = [
        len(items) == universe["candidate_total"] == origin_sum,
        len({item["candidate_id"] for item in items}) == len(items),
        sorted(item["current_rank"] for item in items)
        == list(range(1, len(items) + 1)),
        evidence["compatible_feature_total"]
        == evidence["follow_up"]["compatible_feature_total"]
        + evidence["worth_a_look"]["compatible_feature_total"],
        all(universe["reconciliation"].values()),
    ]

    report = {
        "schema_version": 1,
        "fixture_id": document["fixture_id"],
        "outcome_job_uniqueness_rate": _rate(uniqueness_checks),
        "accepted_outcome_survival_rate": _rate(survival_checks),
        "terminal_idempotency_rate": _rate(idempotency_checks),
        "completed_rank_binding_rate": _rate(binding_checks),
        "stale_cas_rejection_rate": _rate(stale_checks),
        "complete_union_reconciliation_rate": _rate(reconciliation_checks),
        "list_and_page_model_invocations": document["serving_isolation"][
            "list_and_page_model_invocations"
        ],
        "defaults_paused": document["defaults"]["health"]["paused"] is True
        and document["defaults"]["health"]["enabled"] is False,
    }
    report["gate_passed"] = (
        all(
            report[key] == 1.0
            for key in (
                "outcome_job_uniqueness_rate",
                "accepted_outcome_survival_rate",
                "terminal_idempotency_rate",
                "completed_rank_binding_rate",
                "stale_cas_rejection_rate",
                "complete_union_reconciliation_rate",
            )
        )
        and report["list_and_page_model_invocations"] == 0
        and report["defaults_paused"] is True
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    document = _load(args.fixtures)
    report = evaluate(document)
    if report != document["expected_report"]:
        raise SystemExit("rank recompute report does not match frozen expectation")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Content-free Slice-5 off-policy evaluation with grouped bootstrap.

The input rows are one selected position each. Probabilities must be the exact
conditional deployed and target-policy propensities for that position. Each
row also carries the target-policy probability mass covered by the logging
policy over the full candidate set at that decision-position; selected rows
alone cannot diagnose support violations.
"""

import argparse
import json
import math
import random
from pathlib import Path


def percentile(values, quantile):
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * quantile)))
    return ordered[index]


def estimates(rows, switch_threshold):
    weights = []
    ips_terms = []
    dr_terms = []
    switch_terms = []
    support_violations = 0
    supported_target_masses = []
    for row in rows:
        logged = float(row["logged_propensity"])
        target = float(row["target_propensity"])
        reward = float(row["reward"])
        q_logged = float(row["q_logged"])
        q_target = float(row["q_target"])
        supported_target_mass = float(row["target_supported_mass"])
        if not 0.0 < logged <= 1.0 or not 0.0 <= target <= 1.0:
            raise ValueError("propensities must lie in their probability bounds")
        if not 0.0 <= supported_target_mass <= 1.0:
            raise ValueError("target_supported_mass must lie within 0..=1")
        support_violations += int(supported_target_mass < 1.0 - 1e-12)
        supported_target_masses.append(supported_target_mass)
        weight = target / logged
        weights.append(weight)
        ips_terms.append(weight * reward)
        dr_terms.append(q_target + weight * (reward - q_logged))
        switch_terms.append(
            q_target + weight * (reward - q_logged)
            if weight <= switch_threshold
            else q_target
        )
    count = len(rows)
    weight_sum = sum(weights)
    squared_weight_sum = sum(weight * weight for weight in weights)
    return {
        "row_count": count,
        "ips": sum(ips_terms) / count if count else 0.0,
        "snips": (
            sum(weight * float(row["reward"]) for weight, row in zip(weights, rows))
            / weight_sum
            if weight_sum
            else 0.0
        ),
        "doubly_robust": sum(dr_terms) / count if count else 0.0,
        "switch": sum(switch_terms) / count if count else 0.0,
        "effective_sample_size": (
            weight_sum * weight_sum / squared_weight_sum if squared_weight_sum else 0.0
        ),
        "support_violation_count": support_violations,
        "overlap_rate": sum(supported_target_masses) / count if count else 0.0,
        "weight_mean": weight_sum / count if count else 0.0,
        "weight_p50": percentile(weights, 0.50),
        "weight_p95": percentile(weights, 0.95),
        "weight_p99": percentile(weights, 0.99),
        "weight_max": max(weights, default=0.0),
    }


def grouped_bootstrap(rows, group_key, samples, seed, switch_threshold):
    groups = {}
    for row in rows:
        groups.setdefault(str(row[group_key]), []).append(row)
    keys = sorted(groups)
    rng = random.Random(seed)
    metrics = {name: [] for name in ("ips", "snips", "doubly_robust", "switch")}
    for _ in range(samples):
        replay = []
        for _ in keys:
            replay.extend(groups[rng.choice(keys)])
        result = estimates(replay, switch_threshold)
        for name in metrics:
            metrics[name].append(result[name])
    return {
        name: {
            "lower_95": percentile(values, 0.025),
            "upper_95": percentile(values, 0.975),
        }
        for name, values in metrics.items()
    }


def evaluate(document):
    rows = document["rows"]
    settings = document["settings"]
    if settings.get("estimand") != "position_level":
        raise ValueError("this harness requires the explicit position_level estimand")
    identities = [(str(row["decision_id"]), int(row["position"])) for row in rows]
    if any(position < 1 for _, position in identities) or len(set(identities)) != len(identities):
        raise ValueError("decision-position row identities must be positive and unique")
    result = estimates(rows, float(settings["switch_threshold"]))
    result["bootstrap_unit"] = settings["bootstrap_unit"]
    result["bootstrap"] = grouped_bootstrap(
        rows,
        settings["bootstrap_unit"],
        int(settings["bootstrap_samples"]),
        int(settings["bootstrap_seed"]),
        float(settings["switch_threshold"]),
    )
    minimum_ess = float(settings["minimum_effective_sample_size"])
    result["passed"] = (
        result["support_violation_count"] == 0
        and result["effective_sample_size"] >= minimum_ess
        and math.isfinite(result["doubly_robust"])
    )
    return result


def self_test():
    rows = [
        {
            "decision_id": f"d-{index}",
            "session_id": f"s-{index // 2}",
            "position": 1,
            "logged_propensity": 0.5,
            "target_propensity": 0.5,
            "target_supported_mass": 1.0,
            "reward": 1.0,
            "q_logged": 1.0,
            "q_target": 1.0,
        }
        for index in range(20)
    ]
    result = estimates(rows, 10.0)
    for name in ("ips", "snips", "doubly_robust", "switch"):
        if abs(result[name] - 1.0) > 1e-12:
            raise AssertionError(f"{name} synthetic identity check failed")
    if abs(result["effective_sample_size"] - len(rows)) > 1e-12:
        raise AssertionError("ESS synthetic identity check failed")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.fixtures is None:
        parser.error("--fixtures is required unless --self-test is used")
    result = evaluate(json.loads(args.fixtures.read_text()))
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    print(rendered, end="")
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(rendered)
    if not result["passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

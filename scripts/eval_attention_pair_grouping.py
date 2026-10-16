#!/usr/bin/env python3
"""Audit frozen calibrated pair scores with precision-first false-merge metrics."""

import argparse
import json
from pathlib import Path


POSITIVE = {"same_underlying_item", "same_obligation"}


def evaluate(document: dict) -> dict:
    threshold = float(document["audited_merge_threshold"])
    if not 0.5 <= threshold <= 1.0:
        raise ValueError("audited_merge_threshold must be within 0.5..=1.0")
    pairs = document["pairs"]
    predicted = 0
    true_positive = 0
    false_merge = 0
    positive = 0
    negative = 0
    slices: dict[str, dict[str, int]] = {}
    for pair in pairs:
        label = pair["label"]
        if label not in POSITIVE | {"not_duplicate"}:
            raise ValueError(f"unsupported label: {label}")
        probabilities = [
            float(pair["same_underlying_item_probability"]),
            float(pair["same_obligation_probability"]),
        ]
        if any(value < 0.0 or value > 1.0 for value in probabilities):
            raise ValueError("pair probability must be within 0..=1")
        is_positive = label in POSITIVE
        is_merge = max(probabilities) >= threshold
        positive += int(is_positive)
        negative += int(not is_positive)
        predicted += int(is_merge)
        true_positive += int(is_merge and is_positive)
        false_merge += int(is_merge and not is_positive)
        bucket = slices.setdefault(
            pair.get("slice", "unspecified"),
            {"pairs": 0, "predicted_merges": 0, "false_merges": 0},
        )
        bucket["pairs"] += 1
        bucket["predicted_merges"] += int(is_merge)
        bucket["false_merges"] += int(is_merge and not is_positive)
    precision = true_positive / predicted if predicted else 1.0
    recall = true_positive / positive if positive else 0.0
    false_merge_rate = false_merge / negative if negative else 0.0
    return {
        "schema_version": 1,
        "fixture_id": document["fixture_id"],
        "audited_merge_threshold": threshold,
        "pair_count": len(pairs),
        "positive_pair_count": positive,
        "negative_pair_count": negative,
        "predicted_merge_count": predicted,
        "true_positive_merge_count": true_positive,
        "false_merge_count": false_merge,
        "merge_precision": precision,
        "merge_recall": recall,
        "false_merge_rate": false_merge_rate,
        "precision_gate": float(document["precision_gate"]),
        "false_merge_rate_gate": float(document["false_merge_rate_gate"]),
        "passed": precision >= float(document["precision_gate"])
        and false_merge_rate <= float(document["false_merge_rate_gate"]),
        "slices": slices,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    report = evaluate(json.loads(args.fixtures.read_text()))
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    print(rendered, end="")
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(rendered)
    if not report["passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

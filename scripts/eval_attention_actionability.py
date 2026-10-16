#!/usr/bin/env python3
"""Train/select/calibrate/evaluate one immutable Slice-2 model snapshot.

This offline tool is intentionally dependency-free. It consumes frozen,
content-safe numeric feature rows and uses four group-disjoint chronological
partitions: fit, hyperparameter selection, Platt calibration, and final test.
No API-return or no-interaction row is treated as a label.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path

POSITIVE = {"action_completed"}
NEGATIVE = {"irrelevant", "not_actionable", "obsolete", "not_owner"}
EXCLUDED = {"useful", "duplicate_of", "neutral_seen", "timing_negative", "no_interaction"}
L2_GRID = (0.01, 0.1, 1.0, 10.0)


def sigmoid(value: float) -> float:
    if value >= 0:
        return 1.0 / (1.0 + math.exp(-value))
    exp = math.exp(value)
    return exp / (1.0 + exp)


def dot(weights: list[float], row: list[float], intercept: float) -> float:
    return intercept + sum(weight * value for weight, value in zip(weights, row))


def fit_logistic(rows: list[list[float]], labels: list[int], l2: float, steps: int = 1500):
    weights = [0.0] * len(rows[0])
    intercept = 0.0
    for step in range(steps):
        rate = 0.2 / math.sqrt(step + 1.0)
        gradients = [0.0] * len(weights)
        intercept_gradient = 0.0
        for row, label in zip(rows, labels):
            error = sigmoid(dot(weights, row, intercept)) - label
            intercept_gradient += error
            for index, value in enumerate(row):
                gradients[index] += error * value
        scale = 1.0 / len(rows)
        intercept -= rate * intercept_gradient * scale
        for index in range(len(weights)):
            weights[index] -= rate * (gradients[index] * scale + l2 * weights[index])
    return weights, intercept


def fit_platt(logits: list[float], labels: list[int]):
    # Logistic calibration on a group/time-disjoint partition.
    a, b = 1.0, 0.0
    for step in range(1000):
        rate = 0.1 / math.sqrt(step + 1.0)
        da = db = 0.0
        for logit, label in zip(logits, labels):
            error = sigmoid(a * logit + b) - label
            da += error * logit
            db += error
        scale = 1.0 / len(logits)
        a -= rate * da * scale
        b -= rate * db * scale
    return a, b


def brier(probabilities: list[float], labels: list[int]) -> float:
    return sum((probability - label) ** 2 for probability, label in zip(probabilities, labels)) / len(labels)


def ece(probabilities: list[float], labels: list[int], bins: int = 10) -> float:
    total = len(labels)
    value = 0.0
    for index in range(bins):
        low, high = index / bins, (index + 1) / bins
        members = [i for i, probability in enumerate(probabilities) if low <= probability < high or (index == bins - 1 and probability == 1.0)]
        if not members:
            continue
        confidence = sum(probabilities[i] for i in members) / len(members)
        accuracy = sum(labels[i] for i in members) / len(members)
        value += len(members) / total * abs(confidence - accuracy)
    return value


def ranking_metrics(probabilities: list[float], labels: list[int], k: int = 20):
    order = sorted(range(len(labels)), key=lambda index: (-probabilities[index], index))
    top = order[: min(k, len(order))]
    positives = sum(labels)
    return {
        "precision_at_20": sum(labels[index] for index in top) / max(1, len(top)),
        "recall_at_20": sum(labels[index] for index in top) / max(1, positives),
        "brier": brier(probabilities, labels),
        "ece_10": ece(probabilities, labels),
    }


def grouped_temporal_partitions(rows):
    groups = {}
    for row in rows:
        groups.setdefault(row["group_id"], []).append(row)
    ordered = sorted(groups, key=lambda group: max(row["occurred_at"] for row in groups[group]))
    if len(ordered) < 8:
        raise ValueError("at least eight chronological groups are required")
    boundaries = (max(1, int(len(ordered) * 0.50)), max(2, int(len(ordered) * 0.65)), max(3, int(len(ordered) * 0.80)))
    names = ("fit", "selection", "calibration", "test")
    group_slices = (ordered[: boundaries[0]], ordered[boundaries[0] : boundaries[1]], ordered[boundaries[1] : boundaries[2]], ordered[boundaries[2] :])
    partitions = {name: [row for group in selected for row in groups[group]] for name, selected in zip(names, group_slices)}
    if any(not partition for partition in partitions.values()):
        raise ValueError("each grouped temporal partition must be non-empty")
    return partitions


def labelled(raw_rows):
    output = []
    for row in raw_rows:
        outcome = row["outcome"]
        if outcome in EXCLUDED:
            continue
        if outcome not in POSITIVE | NEGATIVE:
            raise ValueError(f"unknown outcome: {outcome}")
        copied = dict(row)
        copied["label"] = int(outcome in POSITIVE)
        output.append(copied)
    return output


def vectorize(rows, feature_names):
    return [[float(row["features"].get(name, 0.0)) for name in feature_names] for row in rows]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", required=True, type=Path)
    parser.add_argument("--snapshot", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()

    fixture_bytes = args.fixtures.read_bytes()
    raw = json.loads(fixture_bytes)
    rows = labelled(raw["rows"])
    partitions = grouped_temporal_partitions(rows)
    feature_names = sorted({name for row in rows for name in row["features"]})
    vectors = {name: vectorize(partition, feature_names) for name, partition in partitions.items()}
    labels = {name: [row["label"] for row in partition] for name, partition in partitions.items()}

    selected = None
    candidates = []
    for l2 in L2_GRID:
        weights, intercept = fit_logistic(vectors["fit"], labels["fit"], l2)
        probabilities = [sigmoid(dot(weights, row, intercept)) for row in vectors["selection"]]
        score = brier(probabilities, labels["selection"])
        candidates.append({"l2_lambda": l2, "selection_brier": score})
        if selected is None or score < selected[0]:
            selected = (score, l2, weights, intercept)
    _, l2, weights, intercept = selected
    calibration_logits = [dot(weights, row, intercept) for row in vectors["calibration"]]
    platt_a, platt_b = fit_platt(calibration_logits, labels["calibration"])
    test_probabilities = [sigmoid(platt_a * dot(weights, row, intercept) + platt_b) for row in vectors["test"]]
    metrics = ranking_metrics(test_probabilities, labels["test"])

    slices = {}
    for slice_name in sorted({row["slice"] for row in partitions["test"]}):
        indexes = [index for index, row in enumerate(partitions["test"]) if row["slice"] == slice_name]
        if indexes:
            slices[slice_name] = ranking_metrics([test_probabilities[i] for i in indexes], [labels["test"][i] for i in indexes])

    dataset_digest = hashlib.sha256(fixture_bytes).hexdigest()
    snapshot_seed = json.dumps({"dataset": dataset_digest, "features": feature_names, "l2": l2}, sort_keys=True).encode()
    snapshot_id = "actionability-" + hashlib.sha256(snapshot_seed).hexdigest()[:16]
    snapshot = {
        "snapshot_id": snapshot_id,
        "model_version": "l2_logistic_platt_v1",
        "feature_contract": "attention_actionability_features_v2",
        "semantic_schema_version": 1,
        "semantic_extractor_contract": raw["semantic_extractor"]["contract"],
        "semantic_prompt_version": raw["semantic_extractor"]["prompt_version"],
        "semantic_model": raw["semantic_extractor"].get("model"),
        "semantic_profile": raw["semantic_extractor"].get("profile"),
        "feature_names": feature_names,
        "coefficients": weights,
        "intercept": intercept,
        "l2_lambda": l2,
        "platt_a": platt_a,
        "platt_b": platt_b,
        "trained_at": raw["frozen_at"],
        "training_manifest": {
            "dataset_digest": dataset_digest,
            "data_cutoff_at": max(row["occurred_at"] for row in rows),
            "split_strategy": "grouped_temporal_fit_selection_calibration_test_v1",
            "group_keys": raw["group_keys"],
            "positive_outcomes": sorted(POSITIVE),
            "negative_outcomes": sorted(NEGATIVE),
            "excluded_outcomes": sorted(EXCLUDED),
            "metrics": metrics,
        },
    }
    report = {
        "schema_version": 1,
        "snapshot_id": snapshot_id,
        "counts": {name: len(partition) for name, partition in partitions.items()},
        "group_overlap": 0,
        "selection_candidates": candidates,
        "metrics": metrics,
        "slices": slices,
        "activation_decision": "review_required",
    }
    args.snapshot.parent.mkdir(parents=True, exist_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.snapshot.write_text(json.dumps(snapshot, indent=2, sort_keys=True) + "\n")
    args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()

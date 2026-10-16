#!/usr/bin/env python3
"""Frozen Slice-4 routing and verified-impression integrity replay."""

import argparse
import json
from pathlib import Path

ROUTES = {"follow_up", "worth_a_look", "non_surfaced"}


def evaluate(document: dict) -> dict:
    minimum_confidence = float(document["policy"]["minimum_route_confidence"])
    minimum_margin = float(document["policy"]["minimum_utility_margin"])
    min_visible_ms = int(document["policy"]["min_visible_ms"])
    max_visible_ms = int(document["policy"]["max_visible_ms"])
    if not 1 <= min_visible_ms <= 60_000:
        raise ValueError("configured min_visible_ms must be within 1..=60000")
    if max_visible_ms != 86_400_000:
        raise ValueError("fixture max_visible_ms must match the serving contract")
    grouping_identity = (
        document["policy"].get("grouping_snapshot_id"),
        document["policy"].get("grouping_model_version"),
    )
    if (grouping_identity[0] is None) != (grouping_identity[1] is None):
        raise ValueError("policy grouping snapshot id and model version must both be present or absent")
    items = {}
    selected_total = violations = actionable = baseline_actionable = learned_actionable = 0
    baseline_irrelevant = learned_irrelevant = 0
    for decision in document["decisions"]:
        rows = decision["items"]
        if len(rows) != int(decision["candidate_count"]):
            raise ValueError("decision candidate total does not reconcile")
        if len({row["candidate_id"] for row in rows}) != len(rows):
            raise ValueError("decision candidate ids are not unique")
        for row in rows:
            if (row.get("grouping_snapshot_id"), row.get("grouping_model_version")) != grouping_identity:
                raise ValueError("candidate grouping identity does not match routing policy")
            if {row["baseline_route"], row["learned_route"], row["served_route"]} - ROUTES:
                raise ValueError("unsupported route")
            identity = (decision["decision_id"], row["candidate_id"])
            items[identity] = row
            selected_total += int(row["selected"])
            changed = row["served_route"] != row["baseline_route"]
            gates_pass = decision["complete_cross_lane_universe"] and float(row["confidence"]) >= minimum_confidence and float(row["margin"]) >= minimum_margin
            violations += int(changed and not gates_pass)
            if row["audited_label"] == "actionable":
                actionable += 1
                baseline_actionable += int(row["baseline_route"] == "follow_up")
                learned_actionable += int(row["served_route"] == "follow_up")
            if row["audited_label"] == "irrelevant_follow_up":
                baseline_irrelevant += int(row["baseline_route"] == "follow_up")
                learned_irrelevant += int(row["served_route"] == "follow_up")

    events = {}
    conflicts = dedupes = 0
    for event in document["impression_events"]:
        if int(event["min_visible_ms"]) != min_visible_ms:
            raise ValueError("impression event dwell policy does not match the decision policy")
        if not 1 <= int(event["visible_ms"]) <= max_visible_ms:
            raise ValueError("impression visible_ms is outside the serving bounds")
        identity = (event["decision_id"], event["candidate_id"])
        if identity not in items or not items[identity]["selected"]:
            raise ValueError("impression does not join to a selected decision item")
        existing = events.get(event["event_id"])
        if existing is None:
            events[event["event_id"]] = dict(event)
        elif (existing["decision_id"], existing["candidate_id"]) != identity:
            conflicts += 1
        else:
            dedupes += 1
            existing["visible_ms"] = max(int(existing["visible_ms"]), int(event["visible_ms"]))
    verified = {(event["decision_id"], event["candidate_id"]) for event in events.values() if int(event["visible_ms"]) >= int(event["min_visible_ms"])}
    coverage = len(verified) / selected_total if selected_total else 0.0
    baseline_recall = baseline_actionable / actionable if actionable else 1.0
    learned_recall = learned_actionable / actionable if actionable else 1.0
    recall_drop = baseline_recall - learned_recall
    irrelevant_reduction = (baseline_irrelevant - learned_irrelevant) / baseline_irrelevant if baseline_irrelevant else 0.0
    gates = document["gates"]
    passed = violations == 0 and conflicts <= int(gates["maximum_event_identity_conflicts"]) and coverage >= float(gates["minimum_verified_impression_coverage"]) and recall_drop <= float(gates["maximum_actionable_recall_drop"]) and irrelevant_reduction >= float(gates["minimum_irrelevant_follow_up_reduction"])
    return {"schema_version": 1, "fixture_id": document["fixture_id"], "candidate_total": len(items), "selected_total": selected_total, "all_candidate_totals_reconcile": len(items) == sum(int(decision["candidate_count"]) for decision in document["decisions"]), "cross_lane_gate_violations": violations, "verified_impression_coverage": coverage, "impression_dedupe_count": dedupes, "event_identity_conflicts": conflicts, "baseline_actionable_recall": baseline_recall, "learned_actionable_recall": learned_recall, "actionable_recall_drop": recall_drop, "irrelevant_follow_up_reduction": irrelevant_reduction, "passed": passed}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", required=True, type=Path)
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

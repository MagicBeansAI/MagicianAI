#!/usr/bin/env python3
"""Deterministic SQL replay for progressive attention-learning activation."""

from __future__ import annotations

import argparse
import json
import math
import sqlite3
from pathlib import Path


def map_mail(case: dict) -> str | None:
    verdict = case["verdict"]
    reason = case.get("reason")
    if verdict == "helpful" and case.get("paired_to_state") == "approved":
        return "action_completed"
    if verdict == "helpful":
        return "useful"
    if verdict == "wrong_label":
        return "not_actionable"
    if verdict != "not_helpful":
        return None
    if reason in {"wrong_classification", "not_actionable"}:
        return "not_actionable"
    if reason == "already_handled":
        return "obsolete"
    if reason in {"delegated", "wrong_owner"}:
        return "not_owner"
    if reason == "duplicate":
        return "duplicate_of"
    return "irrelevant"


def cosine(left: list[float], right: list[float]) -> float:
    dot = sum(a * b for a, b in zip(left, right, strict=True))
    left_norm = math.sqrt(sum(value * value for value in left))
    right_norm = math.sqrt(sum(value * value for value in right))
    return dot / (left_norm * right_norm)


def target_value(outcome: str, task: str) -> bool | None:
    usefulness = {"useful": True, "irrelevant": False, "obsolete": False}
    actionability = {
        "action_completed": True,
        "not_actionable": False,
        "not_owner": False,
    }
    return (usefulness if task == "usefulness" else actionability).get(outcome)


def relevant_knn(document: dict, task: str) -> list[str]:
    contract = document["neighbor_contract"]
    relevant = [
        (cosine(contract["candidate"], label["embedding"]), label["outcome"])
        for label in contract["labels"]
        if target_value(label["outcome"], task) is not None
    ]
    relevant.sort(reverse=True)
    return [outcome for _, outcome in relevant[: contract["k"]]]


def evaluate(document: dict) -> dict:
    connection = sqlite3.connect(":memory:")
    connection.executescript(
        """
        CREATE TABLE checkpoints (
          source TEXT PRIMARY KEY, cutoff_at INTEGER NOT NULL,
          cursor_at INTEGER NOT NULL DEFAULT 0, cursor_id TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE outcomes (
          event_id TEXT PRIMARY KEY, outcome TEXT NOT NULL, occurred_at INTEGER NOT NULL
        );
        """
    )
    cutoff = document["cutoff_at"]
    connection.execute(
        "INSERT OR IGNORE INTO checkpoints(source, cutoff_at) VALUES ('mail', ?)",
        (cutoff,),
    )
    connection.execute(
        "INSERT OR IGNORE INTO checkpoints(source, cutoff_at) VALUES ('mail', ?)",
        (cutoff + 10_000,),
    )

    def import_before_cutoff() -> None:
        for event in sorted(
            (row for row in document["mail_events"] if row["occurred_at"] < cutoff),
            key=lambda row: (row["occurred_at"], row["event_id"]),
        ):
            outcome = map_mail(event)
            if outcome is not None:
                connection.execute(
                    "INSERT OR IGNORE INTO outcomes VALUES (?, ?, ?)",
                    (event["event_id"], outcome, event["occurred_at"]),
                )
            connection.execute(
                """UPDATE checkpoints SET cursor_at = ?, cursor_id = ?
                   WHERE source = 'mail'
                     AND (cursor_at < ? OR (cursor_at = ? AND cursor_id < ?))""",
                (
                    event["occurred_at"],
                    event["event_id"],
                    event["occurred_at"],
                    event["occurred_at"],
                    event["event_id"],
                ),
            )

    import_before_cutoff()
    outcomes_after_first = connection.execute("SELECT COUNT(*) FROM outcomes").fetchone()[0]
    first_cursor = connection.execute(
        "SELECT cursor_at, cursor_id FROM checkpoints WHERE source = 'mail'"
    ).fetchone()
    import_before_cutoff()
    outcomes_after_replay = connection.execute("SELECT COUNT(*) FROM outcomes").fetchone()[0]
    replay_cursor = connection.execute(
        "SELECT cursor_at, cursor_id FROM checkpoints WHERE source = 'mail'"
    ).fetchone()

    for event in document["live_repair_events"]:
        if event["canonical_initially_present"]:
            connection.execute(
                "INSERT OR IGNORE INTO outcomes VALUES (?, ?, ?)",
                (event["event_id"], map_mail(event), event["occurred_at"]),
            )
    before_repair = connection.execute("SELECT COUNT(*) FROM outcomes").fetchone()[0]
    for event in document["live_repair_events"]:
        connection.execute(
            "INSERT OR IGNORE INTO outcomes VALUES (?, ?, ?)",
            (event["event_id"], map_mail(event), event["occurred_at"]),
        )
    after_repair = connection.execute("SELECT COUNT(*) FROM outcomes").fetchone()[0]
    for event in document["live_repair_events"]:
        connection.execute(
            "INSERT OR IGNORE INTO outcomes VALUES (?, ?, ?)",
            (event["event_id"], map_mail(event), event["occurred_at"]),
        )

    baseline_lanes = {
        item["id"]: item["lane"] for item in document["ordering_contract"]["items"]
    }
    learned = sorted(
        document["ordering_contract"]["items"],
        key=lambda item: (item["lane"], -item["score"], item["baseline_rank"]),
    )
    learned_lanes = {item["id"]: item["lane"] for item in learned}
    learned_order = {}
    for item in learned:
        learned_order.setdefault(item["lane"], []).append(item["id"])

    expected_mappings = [event["expected"] for event in document["mail_events"]]
    actual_mappings = [map_mail(event) for event in document["mail_events"]]
    report = {
        "schema_version": 2,
        "fixture_id": document["fixture_id"],
        "mail_mapping_accuracy": sum(
            actual == expected for actual, expected in zip(actual_mappings, expected_mappings, strict=True)
        ) / len(expected_mappings),
        "immutable_cutoff": connection.execute(
            "SELECT cutoff_at FROM checkpoints WHERE source = 'mail'"
        ).fetchone()[0] == cutoff,
        "pre_cutoff_only": all(
            (event["occurred_at"] < cutoff) == (event["event_id"] in {
                row[0] for row in connection.execute("SELECT event_id FROM outcomes")
            })
            for event in document["mail_events"]
            if event["expected"] is not None
        ),
        "replay_idempotent": outcomes_after_first == outcomes_after_replay,
        "cursor_monotonic": replay_cursor >= first_cursor,
        "live_outbox_repaired": after_repair - before_repair
        == sum(not event["canonical_initially_present"] for event in document["live_repair_events"]),
        "live_repair_idempotent": connection.execute("SELECT COUNT(*) FROM outcomes").fetchone()[0]
        == after_repair,
        "task_specific_neighbors": {
            "usefulness": relevant_knn(document, "usefulness"),
            "actionability": relevant_knn(document, "actionability"),
        } == document["neighbor_contract"]["expected"],
        "within_lane_only": baseline_lanes == learned_lanes
        and learned_order == document["ordering_contract"]["expected_order"],
    }
    report["gate_passed"] = all(
        value is True or value == 1.0
        for key, value in report.items()
        if key not in {"schema_version", "fixture_id"}
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    document = json.loads(args.fixtures.read_text(encoding="utf-8"))
    report = evaluate(document)
    if report != document["expected_report"]:
        raise SystemExit(
            "historical attention bootstrap report does not match frozen expectation: "
            + json.dumps(report, sort_keys=True)
        )
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

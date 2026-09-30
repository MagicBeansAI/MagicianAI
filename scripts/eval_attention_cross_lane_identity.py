#!/usr/bin/env python3
"""Content-free audit for exact cross-lane communication-thread reconciliation."""

import argparse
import json
from pathlib import Path
from urllib.parse import unquote_to_bytes


def decode_component(value: str) -> str | None:
    if not value or any(character.isspace() for character in value):
        return None
    try:
        decoded = unquote_to_bytes(value).decode("utf-8")
    except (UnicodeDecodeError, ValueError):
        return None
    if not decoded or any(ord(character) < 32 or ord(character) == 127 for character in decoded):
        return None
    return decoded


def parse_comm_thread(source_ref: str) -> tuple[str, str, str] | None:
    parts = source_ref.split("/")
    if len(parts) != 4:
        return None
    provider, account_alias, thread_id, message_at = parts
    if "@" not in message_at:
        return None
    message_id, occurred_at = message_at.rsplit("@", 1)
    decoded = [decode_component(value) for value in (provider, account_alias, thread_id, message_id)]
    if any(value is None for value in decoded):
        return None
    try:
        if int(occurred_at) <= 0:
            return None
    except ValueError:
        return None
    return decoded[0], decoded[1], decoded[2]


def counts_reconcile(counts: dict) -> bool:
    return all(
        [
            counts["raw_total"]
            == counts["follow_up_raw_total"] + counts["worth_a_look_raw_total"],
            counts["duplicate_hidden_total"] == counts["worth_alias_total"],
            counts["reconciled_total"] == counts["raw_total"],
            counts["raw_total"]
            == counts["materialized_total"] + counts["duplicate_hidden_total"],
            counts["raw_total"]
            == counts["grouped_member_total"] + counts["duplicate_hidden_total"],
            counts["grouped_representative_total"]
            + counts["grouped_duplicate_member_total"]
            == counts["grouped_member_total"],
            counts["lane_total"]
            == counts["follow_up_lane_total"]
            + counts["worth_a_look_lane_total"]
            + counts["non_surfaced_total"],
            counts["lane_total"] == counts["grouped_representative_total"],
            counts["duplicate_exposure_total"] == 0,
        ]
    )


def evaluate(document: dict) -> dict:
    contract = document["contract"]
    scenarios = {case["case"]: case for case in document["scenarios"]}
    exact = scenarios["two_worth_messages_alias_one_active_follow_up_thread"]
    active_follow_threads = {
        tuple(item["typed_thread"][field] for field in contract["typed_components"])
        for item in exact["follow_up"]
        if item["active"]
    }
    resolved_alias_ids = []
    malformed_count = 0
    for worth in exact["worth_a_look"]:
        identity = None
        if worth["source_kind"] == contract["worth_source_kind"]:
            parsed = parse_comm_thread(worth["source_ref"])
            if parsed is None:
                malformed_count += 1
            else:
                typed = {
                    "provider": parsed[0],
                    "account_alias": parsed[1],
                    "thread_id": parsed[2],
                }
                identity = tuple(typed[field] for field in contract["typed_components"])
        if identity in active_follow_threads:
            resolved_alias_ids.append(worth["id"])

    expected_alias_ids = [
        item["id"] for item in exact["worth_a_look"] if item["expected"] == "alias"
    ]
    admitted_ids = set(exact["ranked_grouped_routed_materialized_ids"])
    all_aliases_excluded = all(alias not in admitted_ids for alias in resolved_alias_ids)
    accounting_cases = [case for case in document["scenarios"] if "counts" in case]
    accounting_passed = all(counts_reconcile(case["counts"]) for case in accounting_cases)

    similar = scenarios["similar_content_different_thread_is_not_collapsed"]
    exact_identity_only = (
        similar["follow_up_thread"] != similar["worth_thread"]
        and not similar["expected_alias"]
        and similar["embedding_cosine_similarity"] > 0.99
    )
    malformed = scenarios["malformed_comm_ref_never_collapses"]
    malformed_passed = (
        all(parse_comm_thread(source_ref) is None for source_ref in malformed["refs"])
        and malformed["expected_alias_count"] == 0
        and not malformed["malformed_identity_disclosed"]
        and malformed["reconciliation_status"] == "unavailable"
        and malformed["reason"] == "malformed_worth_comm_source_ref"
        and not malformed["canonical_projection_published"]
        and not malformed["partial_projection_published"]
        and malformed["legacy_worth_guard"]["http_status"] == 503
        and malformed["legacy_worth_guard"]["cards_returned"] == 0
        and malformed["legacy_worth_guard"]["comm_candidates_admitted"] == 0
        and malformed["legacy_worth_guard"]["non_comm_candidates_admitted"] == 0
    )
    reappears = scenarios["worth_alias_reappears_after_follow_up_inactive"]
    reappearance_passed = (
        not reappears["follow_up_active"]
        and reappears["worth_source_active"]
        and reappears["worth_reappeared"]
        and reappears["counts"]["worth_alias_total"] == 0
    )
    digests = scenarios["typed_identity_or_revision_change_changes_digest"]
    digest_values = {
        digests["base_digest"],
        digests["alias_added_digest"],
        digests["owner_revision_changed_digest"],
        digests["origin_revision_changed_digest"],
    }
    digest_passed = len(digest_values) == 4 and all(
        len(value) == 64 and set(value) <= set("0123456789abcdef")
        for value in digest_values
    )

    privacy = document["privacy_contract"]
    privacy_passed = all(
        [
            set(privacy["public_diagnostic_fields"]).isdisjoint(
                privacy["forbidden_public_identity_fields"]
            ),
            not privacy["raw_identity_in_logs"],
            privacy["public_aliases"]["max_items"] == 100,
            privacy["public_aliases"]["identity"] == "canonical_origin_ids",
            not privacy["public_aliases"]["raw_identity_disclosed"],
        ]
    )
    unavailable = scenarios["reconciliation_unavailable_fails_closed"]
    unavailable_passed = all(
        [
            unavailable["reconciliation_status"] == "unavailable",
            unavailable["reason"] == "follow_up_load_unavailable",
            not unavailable["canonical_projection_published"],
            not unavailable["partial_projection_published"],
            unavailable["visible_diagnostic"],
            not unavailable["public_raw_identity_disclosed"],
            unavailable["legacy_worth_guard"]["enforced_server_side"],
            unavailable["legacy_worth_guard"]["http_status"] == 503,
            unavailable["legacy_worth_guard"]["cards_returned"] == 0,
            unavailable["legacy_worth_guard"]["comm_candidates_admitted"] == 0,
            unavailable["legacy_worth_guard"]["non_comm_candidates_admitted"] == 0,
            unavailable["paging"]["duplicate_exposure_total"] == 0,
        ]
    )
    paging = document["paging_contract"]
    paging_passed = all(
        [
            paging["reconcile_before_pagination"],
            paging["canonical_aliases_never_enter_cursor_order"],
            paging["legacy_raw_cursor_advances_across_aliases"],
            not paging["legacy_hidden_aliases_consume_visible_slot"],
            paging["legacy_response_total_uses_reconciled_visible_total"],
            paging["stable_retry_no_skip_or_duplicate"],
            paging["displayed_ids_exact_once_across_pages"],
            paging["server_legacy_worth_guard_required"],
            paging["client_dedupe_is_not_authoritative"],
        ]
    )
    legacy_health = document["legacy_health_contract"]
    legacy_health_passed = all(
        [
            set(legacy_health["global_fields"])
            == {"raw_source_total", "visible_source_total", "duplicate_hidden_total"},
            set(legacy_health["page_fields"])
            == {"raw_scanned_total", "visible_page_total", "duplicate_hidden_page_total"},
            set(legacy_health["equations"])
            == {
                "raw_source_total=visible_source_total+duplicate_hidden_total",
                "raw_scanned_total=visible_page_total+duplicate_hidden_page_total",
                "visible_page_total=returned_card_total",
                "response_total=visible_source_total",
            },
            legacy_health["unavailable_counts"] == "null",
        ]
    )
    gates = {
        "exact_typed_identity_only": exact_identity_only,
        "multiple_exact_aliases_resolved": sorted(resolved_alias_ids)
        == sorted(expected_alias_ids),
        "aliases_excluded_before_learning_and_materialization": all_aliases_excluded,
        "malformed_refs_not_collapsed": malformed_passed,
        "inactive_follow_up_releases_worth_alias": reappearance_passed,
        "all_accounting_equations_reconcile": accounting_passed,
        "identity_alias_and_revision_change_digest": digest_passed,
        "public_diagnostic_content_free": privacy_passed,
        "unavailable_reconciliation_fails_closed": unavailable_passed,
        "paging_exact_once_after_reconciliation": paging_passed,
        "legacy_health_global_and_page_accounting": legacy_health_passed,
    }
    return {
        "schema_version": 1,
        "fixture_id": document["fixture_id"],
        "scenario_count": len(document["scenarios"]),
        "resolved_alias_count": len(resolved_alias_ids),
        "malformed_ref_count": malformed_count,
        "accounting_case_count": len(accounting_cases),
        "gates": gates,
        "passed": all(gates.values()),
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

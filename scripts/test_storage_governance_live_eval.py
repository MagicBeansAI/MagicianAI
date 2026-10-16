from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import unittest


SCRIPT = Path(__file__).with_name("eval-storage-governance-live.py")
SPEC = importlib.util.spec_from_file_location("storage_governance_eval", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
EVAL = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = EVAL
SPEC.loader.exec_module(EVAL)


class StorageGovernanceLiveEvalTests(unittest.TestCase):
    def test_synthetic_contract_covers_every_inventory_and_safety_gate(self) -> None:
        gates = EVAL.synthetic_gates()
        self.assertTrue(gates)
        self.assertTrue(all(gate.passed for gate in gates))
        self.assertIn("mail_lifecycle_boundary", {gate.name for gate in gates})
        self.assertIn("journal_protection", {gate.name for gate in gates})
        self.assertIn("bounded_compaction_metrics", {gate.name for gate in gates})

    def test_snapshot_rejects_mail_retention_or_delete_widening(self) -> None:
        entries = []
        for identifier in sorted(EVAL.EXPECTED_IDS):
            entries.append(
                {
                    "id": identifier,
                    "size_bytes": 0,
                    "allocated_bytes": 0,
                    "wal_bytes": 0,
                    "file_count": 0,
                    "actions": [],
                    "retention_days": 90 if identifier in {"memory_events", "llm_embeddings", "llm_tool_calls"} else None,
                    "safety_class": "authoritative" if identifier == "llm_trace_journal" else "observability",
                }
            )
        mail = next(item for item in entries if item["id"] == "channel_assist_duckdb")
        mail.update(
            safety_class="lifecycle_managed",
            retention_days=90,
            actions=[{"id": "delete_old_mail"}],
        )
        gates = EVAL.validate_snapshot(
            {"principal": "owner", "workspace": "default", "entries": entries},
            "owner",
            "default",
        )
        boundary = next(gate for gate in gates if gate.name == "mail_lifecycle_boundary")
        self.assertFalse(boundary.passed)

    def test_report_contract_requires_all_three_database_owners(self) -> None:
        gates = EVAL.validate_report(
            {
                "started_at_ms": 1,
                "completed_at_ms": 2,
                "duckdb": [{"bytes_after": 1, "row_count": 0}],
            },
            "duckdb",
        )
        self.assertFalse(next(g for g in gates if g.name == "duckdb_all_owners_compacted").passed)

    def test_parquet_report_requires_rolling_canonical_counters(self) -> None:
        payload = {
            "started_at_ms": 1,
            "completed_at_ms": 2,
            "parquet": {
                "partitions_scanned": 1,
                "partitions_compacted": 1,
                "raw_files_pruned": 2,
                "rows_compacted": 2,
            },
            "canonical_llm": {
                "partitions_scanned": 1,
                "partitions_compacted": 0,
                "partitions_with_bounded_tail": 1,
                "raw_files_compacted": 0,
                "raw_tail_files_visible": 3,
                "rows_compacted": 0,
            },
        }
        gates = EVAL.validate_report(payload, "parquet")
        self.assertTrue(next(g for g in gates if g.name == "canonical_llm_rolling_stats").passed)

        del payload["canonical_llm"]["raw_tail_files_visible"]
        gates = EVAL.validate_report(payload, "parquet")
        self.assertFalse(next(g for g in gates if g.name == "canonical_llm_rolling_stats").passed)

    def test_failed_live_contract_still_produces_json_and_html_evidence(self) -> None:
        with TemporaryDirectory() as temporary_directory:
            output_dir = Path(temporary_directory)
            gate = EVAL.Gate("live_contract", False, "service unavailable")
            EVAL.write_report(output_dir, [gate], "live")

            payload = json.loads(
                (output_dir / "report.json").read_text(encoding="utf-8")
            )
            self.assertFalse(payload["passed"])
            self.assertEqual(payload["gates"][0]["detail"], "service unavailable")
            self.assertIn(
                "service unavailable",
                (output_dir / "report.html").read_text(encoding="utf-8"),
            )


if __name__ == "__main__":
    unittest.main()

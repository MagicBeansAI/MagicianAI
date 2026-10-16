#!/usr/bin/env python3
"""Provider-free structural regressions for Phase 2C materialization."""

from __future__ import annotations

import json
import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
CONTRACT_PATH = (
    ROOT
    / "data"
    / "magician_v2"
    / "llm_observability"
    / "phase2-materialization-contract-v1.json"
)
MATERIALIZER_PATH = (
    ROOT
    / "magician"
    / "src"
    / "magician_v2"
    / "analytics"
    / "llm_trace_materializer.rs"
)
JOURNAL_PATH = MATERIALIZER_PATH.with_name("llm_trace_journal.rs")
WORKSPACE_PATH = (
    ROOT
    / "magician"
    / "src"
    / "magician_v2"
    / "artifact_v2"
    / "workspace.rs"
)


class Phase2MaterializationContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
        cls.materializer = MATERIALIZER_PATH.read_text(encoding="utf-8")
        cls.journal = JOURNAL_PATH.read_text(encoding="utf-8")
        cls.workspace = WORKSPACE_PATH.read_text(encoding="utf-8")
        schema = cls.materializer.split(
            'const FACT_COLUMNS: &[(&str, &str)] = &[', 1
        )[1].split(
            "];", 1
        )[0]
        cls.columns = re.findall(r'^\s*\("([a-z0-9_]+)",\s*"[A-Z]+"\),$', schema, re.M)

    def test_materializer_factory_is_activated_only_by_the_phase2f_owner(self) -> None:
        self.assertEqual(
            self.contract["activation"], "active_by_default_phase_2f"
        )
        self.assertIn("pub fn build_canonical_llm_trace_pipeline(", self.materializer)
        callers = []
        for path in (ROOT / "magician" / "src").rglob("*.rs"):
            if path == MATERIALIZER_PATH:
                continue
            if "build_canonical_llm_trace_pipeline(" in path.read_text(
                encoding="utf-8"
            ):
                callers.append(str(path.relative_to(ROOT)))
        self.assertEqual(
            callers,
            ["magician/src/magician_v2/analytics/llm_trace_activation.rs"],
            f"unexpected runtime activation owner: {callers}",
        )

    def test_each_record_kind_has_a_separate_scoped_dataset(self) -> None:
        self.assertEqual(
            self.contract["datasets"],
            {
                "call_fact": "analytics/llm_calls",
                "provider_attempt": "analytics/llm_provider_attempts",
                "capture_gap": "analytics/llm_capture_gaps",
            },
        )
        for method, directory in (
            ("analytics_llm_calls_root", "llm_calls"),
            ("analytics_llm_provider_attempts_root", "llm_provider_attempts"),
            ("analytics_llm_capture_gaps_root", "llm_capture_gaps"),
        ):
            self.assertIn(method, self.workspace)
            self.assertIn(f'.join("{directory}")', self.workspace)
        self.assertIn("record.scope() != scope", self.materializer)

    def test_explicit_schema_contains_every_required_fact_group_and_no_content(self) -> None:
        self.assertGreater(len(self.columns), 100)
        self.assertEqual(len(self.columns), len(set(self.columns)))
        for group, required in self.contract["required_fact_groups"].items():
            with self.subTest(group=group):
                self.assertEqual(set(required) - set(self.columns), set())
        self.assertEqual(
            set(self.contract["forbidden_content_columns"]) & set(self.columns), set()
        )
        production = self.materializer.split("#[cfg(test)]", 1)[0]
        publication = production.split("fn verify_existing_row(", 1)[0]
        self.assertNotIn("SELECT *", publication)
        self.assertIn("DESCRIBE SELECT * FROM read_parquet", production)
        self.assertIn(r'CAST(\"{name}\" AS {ty})', self.materializer)

    def test_publication_is_temporary_fsynced_atomic_and_verified(self) -> None:
        start = self.materializer.index("let result = (|| -> anyhow::Result<()> {")
        write_json = self.materializer.index("write_synced(&json_path", start)
        write_parquet = self.materializer.index("connection", write_json)
        sync_parquet = self.materializer.index("sync_file(&parquet_path)", write_parquet)
        publish = self.materializer.index("std::fs::rename", sync_parquet)
        sync_partition = self.materializer.index("sync_directory(&partition)", publish)
        verify = self.materializer.index("verify_existing_row", sync_partition)
        watermark = self.materializer.index(
            "self.advance_dataset_watermark(dataset, &dataset_root, row)", verify
        )
        self.assertLess(write_json, write_parquet)
        self.assertLess(write_parquet, sync_parquet)
        self.assertLess(sync_parquet, publish)
        self.assertLess(publish, sync_partition)
        self.assertLess(sync_partition, verify)
        self.assertLess(verify, watermark)

    def test_deterministic_object_replay_is_checksum_guarded(self) -> None:
        self.assertEqual(
            self.contract["partitioning"]["object_identity"],
            ["record_kind", "stable_id", "revision"],
        )
        self.assertIn("fn deterministic_file_name(", self.materializer)
        self.assertIn("blake3::hash(row.idempotency_key.as_bytes())", self.materializer)
        self.assertIn("if canonical_file_exists(&final_path)?", self.materializer)
        self.assertNotIn("if final_path.exists()", self.materializer)
        self.assertIn(
            "!canonical_rows_semantically_equal(&actual, expected)",
            self.materializer,
        )
        self.assertIn(
            "fn replay_equality_allows_only_one_ulp_drift_in_validated_money()",
            self.materializer,
        )
        self.assertIn("existing canonical LLM fact", self.materializer)

    def test_batch_is_fully_validated_before_first_publication(self) -> None:
        entry = self.materializer.index("fn materialize(\n        &self,")
        validate = self.materializer.index("self.validate_batch(scope, records)?", entry)
        publish = self.materializer.index("self.materialize_rows(scope, &rows)", validate)
        self.assertLess(validate, publish)
        self.assertIn("journal sequences must be strictly increasing", self.materializer)
        self.assertIn("partition_date(row.occurred_at_ms)", self.materializer)

    def test_each_dataset_has_a_monotonic_checksum_watermark(self) -> None:
        for field in self.contract["dataset_watermark_fields"]:
            self.assertIn(f"pub {field}:", self.materializer)
        self.assertIn('"_materialization-watermark.json"', self.materializer)
        self.assertIn(
            "row.journal_sequence < previous.committed_journal_sequence",
            self.materializer,
        )
        self.assertIn(
            "previous.committed_checksum != row.payload_checksum", self.materializer
        )
        self.assertIn("write_json_atomic_path_sync(path, &watermark)", self.materializer)

    def test_journal_watermark_still_advances_only_after_full_materialization(self) -> None:
        method = self.journal.index("pub fn append_materialize_commit(")
        materialize = self.journal.index("materializer.materialize", method)
        commit = self.journal.index("self.commit_through", materialize)
        self.assertLess(materialize, commit)

    def test_scoped_paths_and_watermarks_cannot_follow_symlinks(self) -> None:
        self.assertEqual(
            self.contract["storage_boundary"]["scoped_directory_components"],
            "real_directories_only_no_symlink_redirection",
        )
        self.assertIn("ensure_real_scoped_directory_chain", self.materializer)
        self.assertIn("ensure_regular_file_or_missing", self.materializer)
        self.assertIn("scoped_directory_symlink_cannot_redirect_materialization", self.materializer)
        self.assertIn("dataset_watermark_symlink_is_rejected_instead_of_followed", self.materializer)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Provider-free structural regressions for Phase 2D governed LLM reads."""

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
    / "phase2-governed-read-contract-v1.json"
)
ANALYTICS = ROOT / "magician" / "src" / "magician_v2" / "analytics"
REGISTRY_PATH = ANALYTICS / "llm_fact_registry.rs"
COMPACTOR_PATH = ANALYTICS / "llm_fact_compactor.rs"
PARQUET_MAINTENANCE_PATH = ANALYTICS / "parquet_maintenance.rs"
SERVICE_PATH = ANALYTICS / "llm_analytics_read_service.rs"
MATERIALIZER_PATH = ANALYTICS / "llm_trace_materializer.rs"
JOURNAL_PATH = ANALYTICS / "llm_trace_journal.rs"
WORKSPACE_PATH = (
    ROOT
    / "magician"
    / "src"
    / "magician_v2"
    / "artifact_v2"
    / "workspace.rs"
)


class Phase2GovernedReadContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
        cls.registry = REGISTRY_PATH.read_text(encoding="utf-8")
        cls.compactor = COMPACTOR_PATH.read_text(encoding="utf-8")
        cls.parquet_maintenance = PARQUET_MAINTENANCE_PATH.read_text(
            encoding="utf-8"
        )
        cls.service = SERVICE_PATH.read_text(encoding="utf-8")
        cls.materializer = MATERIALIZER_PATH.read_text(encoding="utf-8")
        cls.journal = JOURNAL_PATH.read_text(encoding="utf-8")
        cls.workspace = WORKSPACE_PATH.read_text(encoding="utf-8")

    def test_governed_reader_contract_reflects_phase2f_activation(self) -> None:
        self.assertEqual(
            self.contract["activation"], "active_by_default_phase_2f"
        )
        self.assertEqual(self.contract["consumer_wiring_owner"], "phase_2e")
        self.assertNotIn("build_canonical_llm_trace_pipeline", self.service)

    def test_registry_is_exact_content_free_and_derived_from_canonical_schema(self) -> None:
        for relation in self.contract["allowlisted_relations"]:
            self.assertIn(f'"{relation}"', self.registry)
        self.assertIn("FACT_COLUMNS", self.registry)
        self.assertIn("LlmFactContentClass::FactOnly", self.registry)
        self.assertIn("pub fn resolve_name", self.registry)
        self.assertIn("pub fn allows_column", self.registry)
        for forbidden in self.contract["forbidden_fact_columns"]:
            self.assertNotRegex(
                self.materializer,
                re.compile(rf'^\s*\("{re.escape(forbidden)}",\s*"', re.M),
            )

    def test_compaction_is_source_bound_atomic_and_never_deletes_raw(self) -> None:
        expected = self.contract["compaction"]["publication_order"]
        publication_start = self.compactor.index("fn compact_partition(")
        publication_end = self.compactor.index(
            "\nfn usable_manifest(", publication_start
        )
        publication = self.compactor[publication_start:publication_end]
        markers = {
            "fingerprint_sorted_raw_source_set":
                "source_set_checksum_blake3 = source_set_checksum(&sources)?",
            "validate_single_scope_single_record_kind_unique_idempotency_keys":
                "count(DISTINCT idempotency_key)",
            "write_temporary_compacted_parquet": "COPY llm_fact_compact TO",
            "fsync_and_verify_row_count": "sync_file(&tmp)",
            "atomic_rename_compacted_parquet": "std::fs::rename(&tmp, &final_path)",
            "fsync_compaction_directory": "sync_directory(&compact_dir)",
            "checksum_compacted_parquet": "file_checksum(&final_path)",
            "atomically_publish_source_bound_manifest":
                "write_json_atomic_path_sync(compaction_manifest_path(partition), &manifest)",
        }
        positions = [publication.index(markers[step]) for step in expected]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("generation_sources(&raw_files", self.compactor)
        self.assertIn("let actual = source_fingerprint(path)?", self.compactor)
        self.assertNotIn("remove_file(raw", self.compactor)
        self.assertNotIn("remove_dir_all(partition", self.compactor)
        self.assertIn("covered_names.is_subset(&raw_names)", self.compactor)
        self.assertIn(
            "output_checksum != manifest.compacted_checksum_blake3",
            self.compactor,
        )

    def test_partition_selector_is_exclusive_and_filename_namespaces_do_not_mix(self) -> None:
        self.assertIn("canonical_raw_partition_files", self.compactor)
        self.assertIn("legacy_call_partition_files", self.compactor)
        self.assertIn("name.starts_with(&prefix)", self.compactor)
        self.assertIn(
            "super::parquet_maintenance::partition_sources(", self.compactor
        )
        self.assertIn(
            "PartitionedDataset::LegacyLlmCalls", self.compactor
        )
        self.assertIn(
            '!name.starts_with("part-") && !self.is_compacted_file_name(name)',
            self.parquet_maintenance,
        )
        self.assertIn(
            "dataset.is_raw_file_name(name)", self.parquet_maintenance
        )
        self.assertIn("files.push(compacted_file(&partition))", self.compactor)
        self.assertIn(
            "files.extend(usable.uncompacted_tail.iter().cloned())",
            self.compactor,
        )
        self.assertIn("files: raw_files.clone()", self.compactor)
        self.assertNotIn("raw_files.push(compacted_file", self.compactor)

    def test_shared_service_owns_scope_schema_queries_and_formulas(self) -> None:
        self.assertIn("pub struct LlmAnalyticsReadService", self.service)
        self.assertIn("validate_scope(scope)?", self.service)
        self.assertIn("LlmFactRelation::parse(&query.relation)", self.service)
        self.assertIn("validate_columns(&self.registry", self.service)
        self.assertIn("validate_filter(&self.registry", self.service)
        self.assertIn("pub fn overview", self.service)
        self.assertIn("pub fn query_facts", self.service)
        self.assertIn("pub fn read_call", self.service)
        self.assertNotIn("pub fn query_sql", self.service)
        self.assertIn("const DEFAULT_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000", self.service)
        self.assertIn("const MAX_WINDOW_MS: i64 = 31 * DEFAULT_WINDOW_MS", self.service)
        self.assertIn("governed_dataset_sources_in_date_range", self.service)
        self.assertIn("to_ms.saturating_sub(1)", self.service)
        self.assertEqual(
            self.contract["read_service"]["detail_identity_window"],
            "ulid_creation_day_through_full_31_day_bounded_window",
        )
        self.assertIn("Some(start.saturating_add(MAX_WINDOW_MS))", self.service)
        self.assertEqual(
            self.contract["read_service"]["result_decode_failure"],
            "fail_closed_never_coerce_to_unknown_null",
        )
        self.assertIn("let raw_value = row.get::<_, DuckValue>(index)?", self.service)
        self.assertNotIn(".unwrap_or(Value::Null)", self.service)
        self.assertEqual(
            self.contract["read_service"]["canonical_row_integrity"],
            "write_time_content_usage_pricing_timing_validation_capture_and_dataset_lifecycle_invariants_are_revalidated_before_any_governed_projection",
        )
        for guard in (
            "audio_input_tokens IS NULL OR audio_output_tokens IS NULL",
            "cost_source IS NULL AND (input_cost_usd IS NOT NULL",
            "generation_after_ttft_ms IS NOT NULL AND ttft_ms IS NULL",
            "regexp_full_match(CAST(response_kind AS VARCHAR)",
            "validate_revision_lifecycle_consistency",
            "validate_cross_dataset_lifecycle",
            "WITH RECURSIVE",
            "first_missing_at_ms > last_missing_at_ms",
            "attempt_terminal_payload",
        ):
            self.assertIn(guard, self.service)
        self.assertIn(
            "fn canonical_reader_rejects_content_usage_pricing_and_timing_integrity_drift()",
            self.service,
        )
        for regression in (
            "canonical_reader_rejects_immutable_revision_drift",
            "canonical_reader_rejects_cross_dataset_ownership_and_sequence_drift",
            "canonical_reader_rejects_parent_cycles_even_when_each_edge_is_valid",
            "canonical_reader_rejects_preterminal_payloads_and_malformed_gap_windows",
        ):
            self.assertIn(f"fn {regression}()", self.service)
        self.assertIn(
            "LLM_TRACE_FACT_SCHEMA_VERSION.to_string()",
            self.service,
        )

    def test_envelope_freshness_uses_only_the_contiguous_journal_prefix(self) -> None:
        self.assertEqual(
            self.contract["read_service"]["freshness_sequence_authority"],
            "contiguous_durable_journal_materialization_watermark_not_maximum_dataset_watermark",
        )
        self.assertEqual(
            self.contract["read_service"]["freshness_integrity"],
            "watermark_sequence_and_checksum_must_resolve_to_a_verified_immutable_journal_envelope",
        )
        self.assertIn("fn journal_watermark(", self.service)
        self.assertIn("read_verified_journal_watermark", self.service)
        self.assertIn("envelope.sequence == watermark.committed_sequence", self.journal)
        self.assertIn(
            "watermark checksum does not match its journal envelope", self.journal
        )
        self.assertIn(
            "fn envelope_freshness_uses_the_contiguous_journal_prefix()",
            self.service,
        )
        self.assertIn(
            "fn envelope_freshness_rejects_a_shaped_watermark_without_its_journal_envelope()",
            self.service,
        )
        envelope = self.service.split("fn envelope<T: Serialize>", 1)[1].split(
            "fn with_connection", 1
        )[0]
        self.assertNotIn("LlmCanonicalDataset::ALL", envelope)

    def test_latest_revision_projection_coalesces_non_null_facts_and_attempt_identity(self) -> None:
        self.assertIn("arg_max(\\\"{name}\\\", record_revision)", self.service)
        self.assertIn("GROUP BY stable_id", self.service)
        self.assertIn("provider_attempt_index DESC NULLS LAST", self.service)
        for field in (
            "provider_attempt_id",
            "effective_profile",
            "provider",
            "model",
            "attempt_terminal_state",
        ):
            self.assertIn(f'"{field}"', self.service)

    def test_dispatch_timing_keeps_aggregate_and_attempt_ownership_distinct(self) -> None:
        self.assertEqual(
            self.contract["timing_ownership"]["provider_attempt"],
            "dispatch_enrichment_only_when_the_job_has_exactly_one_physical_attempt",
        )
        self.assertEqual(
            self.contract["timing_ownership"]["multi_attempt_terminal_row"],
            "identity_usage_and_economics_retained_but_aggregate_dispatch_latency_is_not_mislabeled_as_final_attempt_latency",
        )
        self.assertIn("d.provider_attempt_count = 1", self.service)
        self.assertIn("LEFT JOIN llm_dispatch_timing d", self.service)
        self.assertIn(
            "fn scoped_dispatch_timing_enriches_queued_attempt_and_call()",
            self.service,
        )

    def test_legacy_projection_is_typed_excluded_and_scope_authoritative(self) -> None:
        self.assertIn("legacy_projection", self.service)
        self.assertIn('"capture_mode" => text("metadata")', self.service)
        self.assertIn('"capture_status" => text("metadata_only")', self.service)
        self.assertIn('text("legacy_schema")', self.service)
        self.assertIn('"training_eligible_at_capture" => "false::BOOLEAN"', self.service)
        self.assertIn('"success" | "transport_success" => "NULL::BOOLEAN"', self.service)
        self.assertIn('| "missing_record_count" => "NULL::UBIGINT"', self.service)
        self.assertIn('| "cache_cost_usd" => "NULL::DOUBLE"', self.service)
        self.assertIn(
            "fn legacy_schema_gaps_remain_unknown_instead_of_fabricating_measurements()",
            self.service,
        )
        self.assertIn("let compatibility_owned = matches!", self.service)
        self.assertIn('| "principal"', self.service)
        self.assertIn('| "workspace"', self.service)
        self.assertIn("analytics_llm_fact_catalog_path", self.workspace)
        self.assertEqual(
            self.contract["stable_projection"]["legacy_capture_defaults"],
            "capture_mode_metadata_capture_status_metadata_only_training_excluded",
        )

    def test_schema_evolution_and_storage_boundaries_fail_closed(self) -> None:
        self.assertIn("missing_optional_fact_columns", self.contract["stable_projection"]["schema_evolution"])
        self.assertEqual(
            self.contract["storage_boundary"]["scoped_directory_components"],
            "real_directories_only_no_symlink_redirection",
        )
        self.assertIn("validate_required_canonical_columns", self.service)
        self.assertIn("ensure_real_scoped_directory_chain", self.compactor + self.service)
        self.assertIn("manifest_symlink_is_ignored_and_governed_read_falls_back_to_raw", self.compactor)

    def test_authored_rust_coverage_names_the_critical_regressions(self) -> None:
        required_tests = (
            "compaction_is_manifest_guarded_preserves_raw_and_keeps_appends_visible",
            "rolling_generation_refuses_to_bless_a_changed_immutable_source",
            "compaction_rejects_scope_mismatch_without_publishing_manifest",
            "compaction_rejects_duplicate_revision_keys",
            "revisions_coalesce_and_terminal_attempt_enriches_logical_call",
            "legacy_rows_are_normalized_without_trusting_embedded_scope",
            "canonical_schema_evolution_projects_missing_columns_as_typed_nulls",
            "scoped_paths_do_not_leak_other_workspace_rows",
            "query_contract_rejects_unknown_relations_columns_and_type_mismatches",
            "envelope_freshness_uses_the_contiguous_journal_prefix",
            "envelope_freshness_rejects_a_shaped_watermark_without_its_journal_envelope",
        )
        combined = self.compactor + self.service
        for test_name in required_tests:
            with self.subTest(test=test_name):
                self.assertIn(f"fn {test_name}()", combined)


if __name__ == "__main__":
    unittest.main()

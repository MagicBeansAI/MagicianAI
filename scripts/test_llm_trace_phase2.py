#!/usr/bin/env python3
"""Provider-free contract regressions for Phase 2 LLM fact recording."""

from __future__ import annotations

import json
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
CONTRACT_PATH = (
    ROOT
    / "data"
    / "magician_v2"
    / "llm_observability"
    / "phase2-fact-contract-v1.json"
)
RECORDER_PATH = (
    ROOT
    / "magician"
    / "src"
    / "magician_v2"
    / "analytics"
    / "llm_trace_recorder.rs"
)


class Phase2FactContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
        cls.source = RECORDER_PATH.read_text(encoding="utf-8")

    def test_contract_is_content_free_and_versioned(self) -> None:
        self.assertEqual(self.contract["contract_version"], 1)
        self.assertEqual(self.contract["fact_schema_version"], 2)
        self.assertEqual(self.contract["journal_schema_version"], 1)
        self.assertEqual(self.contract["maximum_serialized_fact_bytes"], 16 * 1024)
        self.assertTrue(self.contract["content_free"])
        self.assertFalse(self.contract["ordinary_fact_payload_content_allowed"])
        self.assertEqual(
            self.contract["categorical_content_boundary"],
            "operation_capability_routing_error_validation_finish_response_gap_exclusion_and_pricing_provenance_fields_are_short_machine_tokens_not_prose",
        )
        self.assertIn("is_content_free_machine_category", self.source)
        self.assertIn("is_content_free_pricing_version", self.source)
        self.assertIn(
            "fn content_free_categories_reject_prose_and_oversized_values()",
            self.source,
        )
        self.assertIn(
            "pub const MAX_SERIALIZED_LLM_TRACE_RECORD_BYTES: usize = 16 * 1024",
            self.source,
        )
        self.assertIn(
            "fn serialized_fact_size_is_bounded_before_sink_or_replay_acceptance()",
            self.source,
        )

    def test_idempotency_key_and_record_kinds_are_frozen(self) -> None:
        self.assertEqual(
            self.contract["idempotency_key"],
            ["record_kind", "stable_id", "revision"],
        )
        self.assertEqual(
            self.contract["record_kinds"],
            ["call_fact", "provider_attempt", "capture_gap"],
        )

    def test_lifecycle_revisions_do_not_conflate_calls_and_attempts(self) -> None:
        self.assertEqual(
            self.contract["record_revisions"]["call_fact"],
            {"started": 1, "completed": 2},
        )
        self.assertEqual(
            self.contract["record_revisions"]["provider_attempt"],
            {"started": 1, "first_token": 2, "completed": 3},
        )

    def test_attempt_identity_is_explicitly_one_based(self) -> None:
        self.assertEqual(
            self.contract["provider_attempt_id_format"],
            "{llm_call_id}:a{one_based_ordinal}",
        )
        self.assertIn("provider attempt index must be one-based", self.source)
        self.assertIn("provider_attempt_id(record.provider_attempt_index)", self.source)

    def test_capture_status_vocabulary_includes_visible_degradation(self) -> None:
        self.assertEqual(
            self.contract["capture_statuses"],
            [
                "complete",
                "metadata_only",
                "redacted",
                "sampled_out",
                "policy_denied",
                "oversize",
                "backpressure_degraded",
                "write_failed",
            ],
        )

    def test_recorder_validates_before_sink_acceptance(self) -> None:
        submit_start = self.source.index("fn submit(&self, record: LlmTraceRecord)")
        validate = self.source.index("record.validate()?;", submit_start)
        sink = self.source.index("self.sink.try_record(record)?;", submit_start)
        self.assertLess(validate, sink)

    def test_category_validation_is_attached_to_records_that_own_the_fields(self) -> None:
        call_start = self.source.index("fn validate_call_started")
        attempt_start = self.source.index("fn validate_provider_attempt", call_start)
        completion_start = self.source.index("fn validate_call_completed", attempt_start)
        attempt_end = completion_start
        completion_end = self.source.index("fn has_preterminal_attempt_payload", completion_start)

        call_validation = self.source[call_start:attempt_start]
        attempt_validation = self.source[attempt_start:attempt_end]
        completion_validation = self.source[completion_start:completion_end]
        self.assertNotIn("record.error_class", call_validation)
        self.assertNotIn("record.error_code", call_validation)
        self.assertNotIn("record.finish_reason", call_validation)
        for field in ("error_class", "error_code", "finish_reason"):
            self.assertIn(
                f'require_machine_category_option("{field}"', attempt_validation
            )
            self.assertIn(
                f'require_machine_category_option("{field}"', completion_validation
            )

    def test_journal_envelope_has_sequence_identity_and_checksum(self) -> None:
        for field in (
            "journal_schema_version",
            "sequence",
            "key",
            "observed_at_ms",
            "payload_checksum",
            "record",
        ):
            self.assertIn(f"pub {field}:", self.source)
        self.assertIn("blake3::hash", self.source)
        self.assertIn("pub fn verify(&self)", self.source)

    def test_required_scope_and_lineage_identity_is_declared(self) -> None:
        self.assertEqual(
            self.contract["required_common_identity"],
            [
                "trace_id",
                "llm_call_id",
                "principal",
                "workspace",
                "scope_resolution",
                "workload_class",
                "call_role",
            ],
        )
        self.assertIn("pub context: LlmTraceContext", self.source)
        self.assertIn("context.is_valid()", self.source)

    def test_terminal_timing_and_external_aggregate_non_invention_are_frozen(self) -> None:
        self.assertEqual(
            self.contract["terminal_timing"]["completed_call"],
            "created_at_ms_completed_at_ms_and_latency_ms_required",
        )
        self.assertEqual(
            self.contract["terminal_timing"]["completed_attempt"],
            "completion_and_terminal_state_required_exact_start_and_latency_optional_but_paired",
        )
        self.assertEqual(
            self.contract["opaque_external_ai_runs"],
            "not_provider_call_facts_and_must_not_fabricate_attempts",
        )
        self.assertIn("completed attempt requires an exact completion time", self.source)
        self.assertIn(
            "terminal_attempt_allows_unknown_timing_without_inventing_a_start",
            self.source,
        )
        self.assertIn("completed call requires created_at_ms, total latency", self.source)

    def test_token_bucket_integrity_is_enforced_at_the_canonical_boundary(self) -> None:
        self.assertEqual(
            self.contract["token_integrity"]["realtime_modality"],
            "all_audio_and_folded_buckets_required_uncached_audio_plus_cache_never_exceeds_input_and_cache_creation_is_zero",
        )
        self.assertIn(
            "fn realtime_token_buckets_require_a_complete_consistent_modality_split()",
            self.source,
        )
        self.assertIn(
            "uncached audio plus cache buckets must not exceed input_tokens",
            self.source,
        )


if __name__ == "__main__":
    unittest.main()

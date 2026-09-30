#!/usr/bin/env python3
"""Provider-free structural regressions for Phase 3 sanitized capture."""

from __future__ import annotations

import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = json.loads(
    (
        ROOT
        / "data/magician_v2/llm_observability/phase3-sanitized-content-contract-v1.json"
    ).read_text(encoding="utf-8")
)
ANALYTICS = ROOT / "magician/src/magician_v2/analytics"
CONTENT = (ANALYTICS / "llm_trace_content.rs").read_text(encoding="utf-8")
RESTRICTED = (ANALYTICS / "llm_restricted_content.rs").read_text(encoding="utf-8")
RECORDER = (ANALYTICS / "llm_trace_recorder.rs").read_text(encoding="utf-8")
JOURNAL = (ANALYTICS / "llm_trace_journal.rs").read_text(encoding="utf-8")
MATERIALIZER = (ANALYTICS / "llm_trace_materializer.rs").read_text(encoding="utf-8")
REGISTRY = (ANALYTICS / "llm_fact_registry.rs").read_text(encoding="utf-8")
SQL_GUARD = (ANALYTICS / "llm_sql_guard.rs").read_text(encoding="utf-8")
API = (ROOT / "magician-api/src/analytics_api.rs").read_text(
    encoding="utf-8"
)
RUNTIME = (ROOT / "magician-bin/src/main.rs").read_text(encoding="utf-8")
ROUTER = (
    ROOT / "magician/src/magician_v2/query_analysis/operation_llm_router.rs"
).read_text(encoding="utf-8")
MAGICLLM_TYPES = (ROOT / "magicllm/src/types.rs").read_text(encoding="utf-8")
MAGICLLM_ROUTER = (ROOT / "magicllm/src/router.rs").read_text(encoding="utf-8")
CONFIG = (ROOT / "magician/src/config.rs").read_text(encoding="utf-8")
REPO_CONFIG = (ROOT / "magician-config.yaml").read_text(encoding="utf-8")
INTERNAL_DATA = (
    ROOT / "magician/src/magician_v2/execution/embedded_pack_defs/internal_data.yaml"
).read_text(encoding="utf-8")


class Phase3SanitizedCaptureContractTests(unittest.TestCase):
    def test_magicllm_observes_but_magician_owns_policy_and_storage(self) -> None:
        self.assertEqual(CONTRACT["ownership"]["observer"], "magicllm_borrowed_in_process_events_only")
        for event in ("LogicalRequest", "EffectiveRequest", "NormalizedResponse"):
            self.assertIn(event, MAGICLLM_TYPES)
            self.assertIn(f"LlmContentCaptureEvent::{event}", MAGICLLM_ROUTER)
        for local_only in (
            "#[serde(skip, default)]\n    pub content_capture_sink",
            "#[serde(skip, default)]\n    pub logical_content_capture_emitted",
        ):
            self.assertIn(local_only, MAGICLLM_TYPES)
        self.assertNotIn("duckdb", MAGICLLM_TYPES.lower())
        self.assertNotIn("parquet", MAGICLLM_TYPES.lower())
        self.assertIn("set_content_capture_sink", ROUTER)
        self.assertIn("LlmContentCaptureRuntime::start", RUNTIME)

    def test_policy_is_exact_precedence_sampled_and_fail_closed(self) -> None:
        scope = CONTENT.index(".scope_overrides")
        operation = CONTENT.index(".operation_overrides", scope)
        self.assertLess(scope, operation)
        self.assertIn("deterministic_sample(&context.llm_call_id, operation, rate)", CONTENT)
        self.assertIn("exclude_public_guest_content", CONTENT)
        self.assertIn("content_sanitization_failed", CONTENT)
        self.assertIn("record_sanitization_failure_gap", CONTENT)
        self.assertIn("fail_to_metadata_only must remain true", CONFIG)
        self.assertIn("full_local_encrypted LLM capture is disabled", CONFIG)
        self.assertIn("tombstones cannot expire before protected content", CONFIG)
        self.assertEqual(CONTRACT["default_mode"], "metadata")
        self.assertIn("content_mode: metadata", REPO_CONFIG)

    def test_sanitizer_covers_required_privacy_boundaries(self) -> None:
        for marker in (
            "redaction_values()",
            "REDACTED_PRIVATE_KEY",
            "REDACTED_KNOWN_SECRET",
            "BINARY_DATA_URL_REFERENCE",
            "binary_reference_only",
            "content_omitted",
            "raw_provider_body_captured",
            "response_id_fingerprint",
            "content_captured\": false",
            "extra_key_count",
            "MAX_RAW_CAPTURE_IN_FLIGHT_BYTES",
        ):
            self.assertIn(marker, CONTENT)
        self.assertNotIn('"response_id": response.response_id', CONTENT)
        self.assertIn("O_NOFOLLOW", CONTENT)
        self.assertIn("mode & 0o077", CONTENT)
        self.assertEqual(CONTRACT["redaction"]["reasoning"], "count_and_keyed_fingerprint_only")

    def test_restricted_storage_is_separate_and_absent_from_fact_catalog(self) -> None:
        for dataset in CONTRACT["durability"]["datasets"]:
            self.assertIn(dataset, MATERIALIZER)
        self.assertIn("RESTRICTED_CONTENT_COLUMNS", MATERIALIZER)
        fact_columns = MATERIALIZER[
            MATERIALIZER.index("pub const FACT_COLUMNS") : MATERIALIZER.index(
                "pub const RESTRICTED_CONTENT_COLUMNS"
            )
        ]
        for restricted_column in (
            "restricted_payload_json",
            "audit_reason",
            "deletion_reason",
        ):
            self.assertNotIn(restricted_column, fact_columns)
        for dataset in CONTRACT["durability"]["datasets"]:
            self.assertNotIn(f'canonical_name: "{dataset}"', REGISTRY)
        for dataset in CONTRACT["durability"]["datasets"]:
            self.assertNotIn(dataset, SQL_GUARD)
        self.assertIn("LlmTraceJournalNamespace::RestrictedContent", JOURNAL)
        self.assertIn("RESTRICTED_WRITER_LOCK_FILE_NAME", JOURNAL)
        self.assertIn("prune_committed_restricted_prefix", JOURNAL)
        self.assertIn("find_restricted_object_by_stable_name", MATERIALIZER)

    def test_grants_reads_audits_and_deletion_are_fail_closed(self) -> None:
        for marker in (
            "OsRng.fill_bytes",
            "grant_key(&token)",
            "grants.remove(&key)",
            "redaction_unavailable",
            "record_content_access_audit",
            "write_immediate_access_audit",
            "AuditUnavailable",
            "MAX_RESTRICTED_RESPONSE_BYTES",
            "MAX_RESTRICTED_REVISIONS_PER_CALL",
            "write_immediate_tombstone",
            "read_immediate_tombstone_exists",
        ):
            self.assertIn(marker, RESTRICTED)
        self.assertIn("require_setup_token", API)
        self.assertIn("X-Magician-Content-Grant", API)
        self.assertIn('"Cache-Control", "no-store, private"', API)
        self.assertIn("restricted_content_errors_are_fixed_and_never_cacheable", API)
        self.assertNotIn("error: error.to_string()", API[API.index("fn restricted_content_error"):API.index("/// POST /analytics/llm/content/grants")])
        self.assertIn("/analytics/llm/content/grants", RUNTIME)
        self.assertIn("/analytics/llm/content/calls/{llm_call_id}", RUNTIME)
        self.assertEqual(CONTRACT["deletion"]["restart_safe"], True)

    def test_restricted_provider_action_remains_dormant_until_typed_grants_propagate(self) -> None:
        self.assertEqual(
            CONTRACT["restricted_access"]["provider_action"],
            "absent_until_typed_trusted_action_grant_reaches_dispatch",
        )
        self.assertNotIn("read_llm_call_content", INTERNAL_DATA)
        self.assertIn("Fields remain private", RESTRICTED)
        self.assertNotIn("LlmContentReadGrant", INTERNAL_DATA)

    def test_authored_rust_matrix_covers_every_phase3_exit_risk(self) -> None:
        combined = "\n".join((CONTENT, RESTRICTED, JOURNAL, MATERIALIZER, MAGICLLM_ROUTER))
        for test_name in (
            "redacts_known_and_pattern_secrets_before_payload_creation",
            "sanitizer_failure_persists_only_a_content_free_metadata_gap",
            "binary_blocks_are_reference_only",
            "signed_media_urls_are_reference_only_and_never_persisted",
            "dynamic_structured_keys_and_tool_identifiers_cannot_bypass_redaction",
            "prompt_injection_is_inert_data_and_request_fixture_is_exactly_normalized",
            "oversize_payload_degrades_to_explicit_metadata",
            "preclone_oversize_persists_only_content_omission_metadata",
            "cross_scope_and_consumed_grants_fail_closed_and_are_audited",
            "missing_and_forged_grants_are_denied_and_audited_without_storage_reads",
            "restricted_read_reapplies_current_redaction_and_audits_returned_bytes",
            "tombstone_denies_an_already_granted_read_before_any_payload_lookup",
            "restricted_retention_removes_only_expired_utc_partitions",
            "journal_namespaces_reject_cross_lane_records",
            "restricted_journal_prunes_committed_payload_but_keeps_watermark",
            "restricted_replay_is_exact_and_timestamp_drift_cannot_escape_to_another_partition",
            "content_observer_receives_one_logical_effective_and_normalized_event",
        ):
            self.assertIn(f"fn {test_name}", combined)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Provider-free structural regressions for Phase 2B trace durability."""

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
    / "phase2-durability-contract-v1.json"
)
JOURNAL_PATH = (
    ROOT
    / "magician"
    / "src"
    / "magician_v2"
    / "analytics"
    / "llm_trace_journal.rs"
)
RECORDER_PATH = JOURNAL_PATH.with_name("llm_trace_recorder.rs")
WORKSPACE_PATH = (
    ROOT
    / "magician"
    / "src"
    / "magician_v2"
    / "artifact_v2"
    / "workspace.rs"
)
QUEUE_PATH = ROOT / "magicllm" / "src" / "dispatch" / "queue.rs"
DISPATCH_TESTS_PATH = ROOT / "magicllm" / "src" / "dispatch" / "tests.rs"


class Phase2DurabilityContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
        cls.journal = JOURNAL_PATH.read_text(encoding="utf-8")
        cls.index = (JOURNAL_PATH.parent / "llm_trace_journal" / "index.rs").read_text(encoding="utf-8")
        cls.recorder = RECORDER_PATH.read_text(encoding="utf-8")
        cls.workspace = WORKSPACE_PATH.read_text(encoding="utf-8")
        cls.queue = QUEUE_PATH.read_text(encoding="utf-8")
        cls.dispatch_tests = DISPATCH_TESTS_PATH.read_text(encoding="utf-8")

    def test_pipeline_is_additive_and_activated_only_through_phase2f_bridge(self) -> None:
        self.assertEqual(
            self.contract["activation"], "active_by_default_phase_2f"
        )
        self.assertIn("pub struct LlmTraceDurablePipeline", self.journal)
        self.assertNotIn("LlmTraceDurablePipeline::start(", self.recorder)

    def test_priority_channels_are_independent_and_bounded(self) -> None:
        self.assertEqual(
            self.contract["buffer_priority"],
            ["critical", "lineage", "restricted_payload"],
        )
        for channel in (
            "critical_capacity",
            "lineage_capacity",
            "restricted_payload_capacity",
        ):
            self.assertIn(f"pub {channel}: usize", self.journal)
        self.assertGreater(
            self.contract["default_capacity"]["critical"],
            self.contract["default_capacity"]["lineage"],
        )
        self.assertGreater(
            self.contract["default_capacity"]["lineage"],
            self.contract["default_capacity"]["restricted_payload"],
        )
        self.assertIn(
            "self.critical_capacity < self.restricted_payload_capacity",
            self.journal,
        )
        self.assertTrue(
            self.contract["configuration_invariants"]
            ["critical_not_smaller_than_restricted_payload"]
        )
        self.assertIn(
            "fn config_rejects_payload_capacity_larger_than_critical_facts()",
            self.journal,
        )
        self.assertEqual(
            self.contract["configuration_invariants"]["maximum_segment_bytes"],
            64 * 1024 * 1024,
        )
        self.assertTrue(
            self.contract["configuration_invariants"]["bounded_recovery_read"]
        )
        self.assertIn("read_prefix_path_sync", self.workspace)
        self.assertIn("read_bounded_journal_segment", self.journal)
        self.assertIn(
            "fn bounded_segment_reader_rejects_before_loading_an_unbounded_file()",
            self.journal,
        )

    def test_journal_precedes_materialization_and_watermark(self) -> None:
        method = self.journal.index("pub fn append_materialize_commit(")
        append = self.journal.index("self.append_batch(scope, records)?", method)
        replay = self.journal.index("self.replay_uncommitted(scope)?", append)
        materialize = self.journal.index("materializer.materialize", replay)
        commit = self.journal.index("self.commit_through", materialize)
        self.assertLess(append, replay)
        self.assertLess(replay, materialize)
        self.assertLess(materialize, commit)

    def test_stable_revision_conflicts_fail_closed(self) -> None:
        self.assertEqual(
            self.contract["idempotency_key"],
            ["record_kind", "stable_id", "revision"],
        )
        self.assertIn("IdempotencyConflict", self.journal)
        self.assertIn("existing == &checksum", self.journal)
        self.assertIn("different payload", self.journal)

    def test_restart_replay_is_checksum_and_sequence_guarded(self) -> None:
        self.assertIn("pub fn replay_uncommitted(", self.journal)
        # JSONL decoding and checksum verification are deliberately one
        # operation so no recovery caller can deserialize an unverified
        # envelope and forget the integrity check.
        self.assertIn("LlmTraceJournalEnvelope::from_json_line_verified(line)?", self.journal)
        self.assertIn("envelope.verify()?", self.recorder)
        self.assertIn("envelope.sequence != expected", self.journal)
        self.assertIn("expected sequence {expected}", self.journal)
        self.assertIn("validate_watermark(", self.journal)
        self.assertIn("&watermark,\n            &envelopes,", self.journal)
        self.assertEqual(
            self.contract["recovery"]["materialization_failure"],
            "retain_uncommitted_watermark_and_replay",
        )
        self.assertEqual(
            self.contract["recovery"]["lifecycle_delivery_order"],
            "independent_immutable_revisions_accept_missing_or_late_predecessors_and_revalidate_every_resolved_parent_edge_while_context_operation_effective_route_timing_and_attempt_count_drift_remain_fail_closed",
        )
        self.assertIn(
            "fn missing_or_late_lifecycle_revisions_do_not_poison_scope_flushes()",
            self.journal,
        )
        self.assertNotIn(
            "call completion revision requires an earlier start revision",
            self.journal,
        )
        self.assertIn("for child_id in audit.parents.keys()", self.journal)
        self.assertIn(
            "fn late_parent_with_a_different_trace_is_rejected_before_append()",
            self.journal,
        )
        self.assertIn(
            "fn late_parent_with_the_same_trace_repairs_the_unresolved_edge()",
            self.journal,
        )
        self.assertIn(
            "fn lifecycle_effective_route_drift_is_rejected_before_append()",
            self.journal,
        )

    def test_startup_recovery_cannot_leave_a_detached_competing_writer(self) -> None:
        self.assertEqual(
            self.contract["recovery"]["startup_barrier"],
            "wait_for_writer_index_validation_then_materialize_bounded_batches_in_background_without_detaching_a_live_writer",
        )
        self.assertEqual(
            self.contract["recovery"]["writer_ownership"],
            "one_cross_process_advisory_lease_held_until_the_blocking_worker_really_exits",
        )
        self.assertIn("match startup_rx.recv()", self.journal)
        self.assertNotIn("startup_rx.recv_timeout", self.journal)
        self.assertIn("acquire_writer_lock(&workspace, config.namespace)?", self.journal)
        self.assertIn("_writer_lock: File", self.journal)
        self.assertIn(
            "fn writer_lease_rejects_a_second_pipeline_owner()", self.journal
        )
        self.assertIn(
            "fn writer_lease_symlink_is_rejected_instead_of_followed()",
            self.journal,
        )
        self.assertIn("libc::O_NOFOLLOW", self.journal)
        self.assertIn(
            "journal worker terminated before startup recovery completed",
            self.journal,
        )
        self.assertIn("materializer.materialize(scope, &uncommitted)?", self.journal)
        self.assertIn("self.commit_through(scope, last.sequence)?", self.journal)
        self.assertIn(
            "fn recovery_scope_is_materialized_and_committed_by_flush_barrier()",
            self.journal,
        )
        self.assertIn(
            "fn materialization_failure_keeps_backlog_on_disk_without_blocking_writer_readiness()",
            self.journal,
        )

    def test_only_partial_final_jsonl_tail_is_repaired(self) -> None:
        self.assertIn("complete_jsonl_prefix_len", self.journal)
        self.assertIn("non-final segment", self.journal)
        self.assertEqual(
            self.contract["recovery"]["malformed_complete_line"], "fail_closed"
        )
        self.assertEqual(
            self.contract["recovery"]["segment_filename"],
            "validated_fixed_width_first_sequence_before_replay_or_reuse",
        )
        self.assertEqual(
            self.contract["recovery"]["oversized_segment"],
            "reject_before_unbounded_read_or_json_decode",
        )
        self.assertIn(
            "fn recovery_rejects_malformed_sequence_bearing_segment_names()",
            self.journal,
        )

    def test_critical_overload_has_exact_grouped_gap_accounting(self) -> None:
        self.assertEqual(
            self.contract["critical_overload"]["counter_dimensions"],
            ["principal", "workspace", "operation", "reason"],
        )
        for field in (
            "scope: LlmScope",
            "operation: String",
            "reason: String",
            "missing_record_count: u64",
        ):
            self.assertIn(field, self.journal)
        self.assertIn("try_emit_pending_gap", self.journal)
        self.assertIn("take_pending_gap_records", self.journal)

    def test_shutdown_is_bounded_prioritized_and_reports_remainder(self) -> None:
        self.assertTrue(self.contract["shutdown"]["bounded"])
        self.assertEqual(
            self.contract["shutdown"]["drain_order"],
            ["critical", "lineage", "restricted_payload"],
        )
        self.assertIn("tokio::time::timeout(self.shutdown_timeout", self.journal)
        self.assertIn("pub remaining_buffered_records: u64", self.journal)
        self.assertIn("pub remaining_missing_records: u64", self.journal)
        self.assertEqual(
            self.contract["shutdown"]["owner_disconnect"],
            "close_admission_then_defensively_drain_and_flush_the_stable_accepted_prefix",
        )
        self.assertIn("impl Drop for LlmTraceDurablePipeline", self.journal)
        self.assertIn(
            "LLM trace journal owner disappeared and the defensive final flush failed",
            self.journal,
        )
        self.assertIn(
            "fn dropped_owner_closes_admission_and_defensively_flushes_accepted_records()",
            self.journal,
        )

    def test_journal_has_a_scope_owned_workspace_path(self) -> None:
        self.assertIn("analytics_llm_trace_journal_root", self.workspace)
        self.assertIn('.join("llm_trace_journal")', self.workspace)
        self.assertIn("record.scope() != scope", self.journal)

    def test_scope_storage_and_dispatch_admission_fail_closed(self) -> None:
        self.assertEqual(
            self.contract["storage_boundary"]["tenant_path_components"],
            "real_directories_only_no_symlink_redirection",
        )
        self.assertEqual(
            self.contract["upstream_dispatch_admission"]["shutdown_wins_after_intent"],
            "submitted_then_tombstoned_zero_attempt_lifecycle_without_ghost_pending_job",
        )
        self.assertEqual(
            self.contract["upstream_dispatch_admission"]["expired_submission"],
            "rejected_before_idempotency_lookup_or_registration",
        )
        self.assertEqual(
            self.contract["upstream_dispatch_admission"]["expired_submission_telemetry"],
            "submitted_then_tombstoned_with_zero_provider_attempts_for_sync_and_stream",
        )
        self.assertIn("validate_scope_storage", self.journal)
        self.assertIn("journal_scope_symlink_cannot_redirect_durable_writes", self.journal)
        self.assertIn("journal_watermark_symlink_is_rejected_instead_of_followed", self.journal)

        submit = self.queue.index("pub async fn submit(&self, mut job: LlmJob)")
        ledger = self.queue.index("LlmCallLedgerEvent::Submitted", submit)
        admission = self.queue.index("let _admission = self.inner.admission.lock()", ledger)
        pending = self.queue.index("self.inner.registry.insert_pending", admission)
        lane = self.queue.index("lane.try_push(job)", pending)
        deadline = self.queue.index(".submission_deadline", submit)
        idempotency = self.queue.index("lookup_or_register(&key", submit)
        self.assertLess(deadline, idempotency)
        self.assertLess(ledger, admission)
        self.assertLess(admission, pending)
        self.assertLess(pending, lane)

        stream = self.queue.index("pub async fn submit_stream(&self, mut job: LlmStreamJob)")
        stream_ledger = self.queue.index("LlmCallLedgerEvent::Submitted", stream)
        stream_admission = self.queue.index(
            "let _admission = self.inner.admission.lock()", stream_ledger
        )
        stream_pending = self.queue.index(
            "self.inner.registry.insert_pending", stream_admission
        )
        stream_spawn = self.queue.index("tasks.push(dispatch_stream_job", stream_pending)
        self.assertLess(stream_ledger, stream_admission)
        self.assertLess(stream_admission, stream_pending)
        self.assertLess(stream_pending, stream_spawn)

        for regression in (
            "shutdown_does_not_observe_sync_submission_blocked_on_ledger_as_pending",
            "shutdown_does_not_observe_stream_submission_blocked_on_ledger_as_pending",
            "expired_submission_never_reuses_cached_idempotent_result",
            "expired_stream_submission_is_rejected_and_emits_zero_attempt_tombstone",
            "in_flight_cancellation_preserves_the_started_provider_attempt_identity",
        ):
            self.assertIn(f"async fn {regression}()", self.dispatch_tests)


if __name__ == "__main__":
    unittest.main()

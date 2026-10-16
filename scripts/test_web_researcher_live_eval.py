#!/usr/bin/env python3
"""Deterministic contract tests for the web-researcher live evaluator."""

from __future__ import annotations

import importlib.util
import gzip
import json
import sys
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import Mock, patch


REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = Path(__file__).with_name("eval-web-researcher-live.py")
SPEC = importlib.util.spec_from_file_location("web_researcher_live_eval", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class WebResearcherLiveEvalTests(unittest.TestCase):
    def test_live_task_roots_accept_an_execution_local_named_profile(self) -> None:
        self.assertEqual(
            MODULE.eval_execution_request("candidate-profile"),
            {
                "llm_routing_overrides": {
                    "planning": {"profile": "candidate-profile"},
                    "evaluation": {"profile": "candidate-profile"},
                    "correction_extraction": {
                        "profile": "candidate-profile"
                    },
                    "memory_consolidation": {
                        "profile": "candidate-profile"
                    },
                    "operations": {
                        "agentic_decision": {"profile": "candidate-profile"},
                        "evidence_precision_judge": {
                            "profile": "candidate-profile"
                        },
                        "durable_task_state_generate": {
                            "profile": "candidate-profile"
                        },
                        "durable_task_state_patch": {
                            "profile": "candidate-profile"
                        },
                        "durable_task_state_close_summary": {
                            "profile": "candidate-profile"
                        },
                    }
                }
            },
        )
        self.assertEqual(
            MODULE.DEFAULT_EVAL_LLM_PROFILE, "gpt6luna-responses-toolsany"
        )
        delegated = MODULE.eval_execution_request(
            "candidate-profile", "web-researcher"
        )
        self.assertEqual(delegated["delegate_to_agent"], "web-researcher")

    def test_fixture_covers_direct_and_delegated_paths_and_pricing_question(self) -> None:
        cases = MODULE.load_cases(
            REPO_ROOT / "scripts/fixtures/web_researcher_live/cases.json"
        )
        self.assertEqual({case["mode"] for case in cases}, {"direct", "delegated"})
        self.assertTrue(all("max_answer_ready_seconds" not in case for case in cases))
        pricing = next(case for case in cases if case["mode"] == "direct")
        self.assertEqual(
            pricing["query"],
            "Compare prices of GPT 5.6 family and Sarvam.",
        )
        self.assertEqual(pricing["required_tool_groups"], [["content_search"]])
        delegated = next(case for case in cases if case["mode"] == "delegated")
        self.assertNotEqual(delegated["root_agent_id"], "web-researcher")
        self.assertIn("web-researcher", delegated["query"])
        self.assertIn("one content_search call", delegated["query"])
        self.assertIn("requests array", delegated["query"])

    def test_invalid_fixture_fails_closed(self) -> None:
        fixture = {
            "schema_version": 1,
            "cases": [
                {
                    "id": "bad",
                    "mode": "delegated",
                    "root_agent_id": "personal-assistant",
                    "query": "Search the web",
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "cases.json"
            path.write_text(json.dumps(fixture), encoding="utf-8")
            with self.assertRaisesRegex(MODULE.EvalFailure, "explicitly request"):
                MODULE.load_cases(path)

    def test_html_and_markdown_citations_are_normalized_and_deduplicated(self) -> None:
        raw = b"""
        <html><head><style>hidden</style></head><body>
          <p>Python stable release.</p>
          <a href="https://www.python.org/downloads/">Official downloads</a>
          <p>[same](https://www.python.org/downloads/)</p>
        </body></html>
        """
        text, citations = MODULE.text_and_citations(raw, "text/html")
        self.assertIn("Python stable release", text)
        self.assertNotIn("hidden", text)
        self.assertEqual(citations, ["https://www.python.org/downloads/"])

    def test_authority_matching_is_suffix_bounded(self) -> None:
        self.assertTrue(MODULE.host_matches("blog.rust-lang.org", "rust-lang.org"))
        self.assertTrue(MODULE.host_matches("python.org", "python.org"))
        self.assertFalse(MODULE.host_matches("rust-lang.org.attacker.example", "rust-lang.org"))

    def test_required_term_normalization_accepts_rich_typographic_hyphens(self) -> None:
        answer = "GPT‑5.6 Luna and Sarvam‑105B pricing"  # U+2011 hyphens
        normalized = MODULE.normalize_term_text(answer)
        self.assertIn("gpt-5.6", normalized)
        self.assertIn("sarvam-105b", normalized)
        self.assertEqual(
            MODULE.normalize_term_text("GPT－5.6"),  # full-width hyphen-minus
            "gpt-5.6",
        )

    def test_oversized_citation_retains_bounded_prefix_for_judge(self) -> None:
        self.assertEqual(MODULE.MAX_CITATION_EVIDENCE_BYTES, 4 * 1024 * 1024)
        response = Mock()
        response.status = 200
        response.read.return_value = (
            b"<html><head><title>Official releases</title></head>"
            b"<body><p>Rust 1.97.1 was released July 16, 2026.</p>"
            + b"x" * 256
        )
        response.geturl.return_value = "https://doc.rust-lang.org/stable/releases.html"
        response.headers.get_content_type.return_value = "text/html"
        response.headers.get_content_charset.return_value = "utf-8"
        opener = Mock()
        opener.open.return_value = response

        with (
            patch.object(MODULE, "MAX_CITATION_EVIDENCE_BYTES", 128),
            patch.object(MODULE, "build_opener", return_value=opener),
        ):
            probe = MODULE.verify_citation(
                "https://doc.rust-lang.org/stable/releases.html"
            )

        self.assertEqual(probe.status, 200)
        self.assertEqual(probe.title, "Official releases")
        self.assertIn("Rust 1.97.1", probe.excerpt)
        self.assertIn("retained bounded prefix", probe.error or "")

    def test_citation_reader_decodes_gzip_and_both_deflate_forms(self) -> None:
        payload = b"<html><body>Current official price is $1.25.</body></html>"
        raw_deflater = zlib.compressobj(wbits=-zlib.MAX_WBITS)
        encoded = {
            "gzip": gzip.compress(payload),
            "deflate": zlib.compress(payload),
            "raw-deflate": raw_deflater.compress(payload) + raw_deflater.flush(),
        }
        for label, body in encoded.items():
            content_encoding = "deflate" if label == "raw-deflate" else label
            with self.subTest(encoding=label):
                decoded, truncated = MODULE.decode_http_content(
                    body, content_encoding, limit=1024
                )
                self.assertEqual(decoded, payload)
                self.assertFalse(truncated)

    def test_citation_reader_bounds_compressed_expansion(self) -> None:
        decoded, truncated = MODULE.decode_http_content(
            gzip.compress(b"x" * 4096), "gzip", limit=128
        )
        self.assertEqual(decoded, b"x" * 128)
        self.assertTrue(truncated)

    def test_wait_for_answer_fails_fast_on_unattended_user_pause(self) -> None:
        executions = [
            {
                "execution_id": "exec-1",
                "agent_id": "web-researcher",
                "status": "waiting_for_user",
            }
        ]
        with patch.object(MODULE, "list_executions", return_value=(executions, 1.0)):
            with self.assertRaisesRegex(MODULE.EvalFailure, "waiting_for_user"):
                MODULE.wait_for_answer(object(), "task-1", "exec-1")

    def test_nested_agent_event_tool_lifecycle_is_paired(self) -> None:
        events = [
            {
                "event_type": "AgentEvent",
                "data": {
                    "event": {
                        "event_type": "tool.call.started",
                        "data": {"call_id": "call-1", "tool_name": "content_search"},
                    }
                },
            },
            {
                "event_type": "AgentEvent",
                "data": {
                    "event": {
                        "event_type": "tool.call.finished",
                        "data": {
                            "call_id": "call-1",
                            "tool_name": "content_search",
                            "success": True,
                            "duration_ms": 42,
                        },
                    }
                },
            },
        ]
        calls, paired = MODULE.tool_calls_from_events(events)
        self.assertEqual(paired, 1)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0].tool_name, "content_search")
        self.assertTrue(calls[0].success)

    def test_runtime_phase_timings_are_collected_from_nested_events(self) -> None:
        events = [
            {
                "event_type": "delegation.launch.dispatched",
                "payload": {
                    "phase_timing_ms": {
                        "admission_queue": 3,
                        "admission_preflight": 18,
                        "resource_wait": 41,
                    }
                },
            },
            {
                "event_type": "execution.runtime.resume.handoff",
                "data": {
                    "event": {
                        "mode": "delegation_checkpoint_delta",
                        "resume_ms": 7,
                    }
                },
            },
        ]

        self.assertEqual(
            MODULE.phase_timings_from_events(events),
            {
                "admission_queue": 3.0,
                "admission_preflight": 18.0,
                "resource_wait": 41.0,
                "delegation_checkpoint_resume": 7.0,
            },
        )

    def test_terminal_lifecycle_projection_and_auxiliary_gates(self) -> None:
        events = [
            {
                "event_type": "agentic.execution_completed",
                "execution_id": "exec-root",
                "timestamp": "2026-08-03T10:00:00Z",
                "payload": {"outcome": "success"},
            },
            {
                "event_type": "execution.status_changed",
                "execution_id": "exec-root",
                "payload": {
                    "new_status": "completed",
                    "timestamp_ms": 1_786_267_201_000,
                },
            },
        ]
        result = MODULE.CaseResult(
            case_id="lifecycle#1",
            fixture_id="lifecycle",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            task_id="task-1",
            root_execution_id="exec-root",
            execution_ids=["exec-root"],
            root_status="completed",
            output_id="out-task-1",
            settled_events=events,
            terminal_lifecycle_events=MODULE.terminal_lifecycle_events(events),
            llm_calls=[
                {
                    "operation": "agentic_decision",
                    "execution_id": "exec-root",
                    "input_tokens": 10_000,
                    "success": True,
                },
                {
                    "operation": "agentic_decision",
                    "execution_id": "exec-root",
                    "input_tokens": 13_000,
                    "success": True,
                },
                {
                    "operation": "task_summary",
                    "execution_id": "exec-root",
                    "root_execution_id": "exec-root",
                    "timestamp_ms": 1_786_267_202_000,
                    "success": True,
                },
            ],
        )
        long_answer = "Supported result. " * 30
        gates = MODULE.lifecycle_gates(result, long_answer)
        self.assertTrue(all(gate.passed for gate in gates), gates)
        self.assertGreater(result.terminal_lifecycle_events[0]["timestamp_ms"], 0)

    def test_lifecycle_requires_one_matching_terminal_for_root_and_agentic_child(self) -> None:
        events = [
            {"event_type": "execution.completed", "execution_id": "exec-root"},
            {"event_type": "agentic.execution_completed", "execution_id": "exec-child"},
            {"event_type": "execution.completed", "execution_id": "exec-child"},
        ]
        terminals = MODULE.terminal_lifecycle_events(events)
        result = MODULE.CaseResult(
            case_id="delegated-lifecycle#1",
            fixture_id="delegated-lifecycle",
            repeat=1,
            mode="delegated",
            root_agent_id="personal-assistant",
            root_execution_id="exec-root",
            execution_ids=["exec-root", "exec-child"],
            root_status="completed",
            settled_events=events,
            terminal_lifecycle_events=terminals,
        )

        def terminal_gate():
            return next(
                gate for gate in MODULE.lifecycle_gates(result, "Short supported answer.")
                if gate.name == "lifecycle.one_agentic_terminal_per_execution"
            )

        self.assertTrue(terminal_gate().passed)
        # The root did not run an agentic loop; the child did. Each requires
        # exactly one matching terminal, even if the child has a lifecycle
        # projection as well.
        for index in (0, 1):
            with self.subTest(execution=events[index]["execution_id"], defect="missing"):
                result.terminal_lifecycle_events = terminals[:index] + terminals[index + 1:]
                self.assertFalse(terminal_gate().passed)
            with self.subTest(execution=events[index]["execution_id"], defect="duplicate"):
                result.terminal_lifecycle_events = terminals + [terminals[index]]
                self.assertFalse(terminal_gate().passed)

    def test_lifecycle_gate_rejects_terminal_replay_after_summary(self) -> None:
        result = MODULE.CaseResult(
            case_id="replay#1",
            fixture_id="replay",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            root_execution_id="exec-root",
            execution_ids=["exec-root"],
            root_status="completed",
            output_id="out-1",
            terminal_lifecycle_events=[
                {
                    "event_type": "agentic.execution.completed",
                    "execution_id": "exec-root",
                    "status": "success",
                    "timestamp_ms": 300,
                }
            ],
            llm_calls=[
                {
                    "operation": "task_summary",
                    "execution_id": "exec-root",
                    "root_execution_id": "exec-root",
                    "timestamp_ms": 200,
                    "success": True,
                }
            ],
        )
        gates = MODULE.lifecycle_gates(result, "Supported result. " * 30)
        replay = next(
            gate
            for gate in gates
            if gate.name == "lifecycle.no_terminal_replay_after_task_summary"
        )
        self.assertFalse(replay.passed)

    def test_cache_usage_gate_rejects_impossible_provider_buckets(self) -> None:
        result = MODULE.CaseResult(
            case_id="cache#1",
            fixture_id="cache",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            llm_calls=[
                {
                    "success": True,
                    "operation": "agentic_decision",
                    "profile": "cached-profile",
                    "input_tokens": 100,
                    "cache_read_tokens": 80,
                    "cache_creation_tokens": 30,
                }
            ],
        )

        gate = MODULE.cache_usage_accounting_gate(result)
        self.assertFalse(gate.passed)
        self.assertIn("cached-profile", gate.detail)

    def test_canonical_tool_lineage_is_paired_and_timed(self) -> None:
        def lineage(stage: str, occurred_at_ms: int, **extra):
            metadata = {
                "stage": stage,
                "tool_execution_id": "llm-1:tool:call-1",
                "tool_name": "content_search",
                "occurred_at_ms": occurred_at_ms,
                "outcome": "pending" if stage == "execution_started" else "succeeded",
                **extra,
            }
            return {
                "event_type": "llm.tool.lineage",
                "kind": {"kind": "agent_notification", "metadata": metadata},
            }

        events = [
            lineage("proposed", 90),
            lineage("execution_started", 100),
            lineage("execution_finished", 142, tool_reported_success=True),
            lineage("result_validated", 143),
        ]
        calls, paired = MODULE.tool_calls_from_events(events)
        self.assertEqual(paired, 1)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0].tool_name, "content_search")
        self.assertEqual(calls[0].duration_ms, 42)
        self.assertTrue(calls[0].success)

    def test_canonical_tool_fact_rows_are_paired_and_classify_failures(self) -> None:
        rows = [
            {
                "observed_at_ms": 100,
                "tool_execution_id": "llm-1:tool:call-1",
                "tool_name": "content_search",
                "tool_lineage_stage": "execution_started",
                "tool_outcome": "pending",
            },
            {
                "observed_at_ms": 142,
                "tool_execution_id": "llm-1:tool:call-1",
                "tool_name": "content_search",
                "tool_lineage_stage": "execution_finished",
                "tool_outcome": "succeeded",
                "tool_reported_success": True,
            },
            {
                "observed_at_ms": 200,
                "tool_execution_id": "llm-2:tool:call-2",
                "tool_name": "content_read",
                "tool_lineage_stage": "execution_started",
                "tool_outcome": "pending",
            },
            {
                "observed_at_ms": 225,
                "tool_execution_id": "llm-2:tool:call-2",
                "tool_name": "content_read",
                "tool_lineage_stage": "execution_finished",
                "tool_outcome": "timed_out",
                "tool_reported_success": False,
                "tool_failure_code": "runtime_timeout",
            },
        ]
        calls, paired = MODULE.tool_calls_from_fact_rows(rows)
        self.assertEqual(paired, 2)
        self.assertEqual([call.tool_name for call in calls], ["content_search", "content_read"])
        self.assertEqual(calls[0].duration_ms, 42)
        self.assertTrue(calls[0].success)
        self.assertFalse(calls[1].success)
        self.assertEqual(calls[1].error, "runtime_timeout")

    def test_retrieval_failure_is_recovered_only_by_later_same_tool_success(self) -> None:
        calls = [
            MODULE.ToolCall("search-1", "content_search", False, error="provider_error"),
            MODULE.ToolCall("read-1", "content_read", False, error="approval_required"),
            MODULE.ToolCall("read-2", "content_read", True),
            MODULE.ToolCall("browser-1", "browser", False, error="dispatch_error"),
        ]
        unrecovered, recovered = MODULE.unrecovered_tool_failures(calls)
        self.assertEqual(recovered, 1)
        self.assertEqual(
            [(call.tool_name, call.error) for call in unrecovered],
            [
                ("content_search", "provider_error"),
                ("browser", "dispatch_error"),
            ],
        )

    def test_supported_answer_supersedes_failed_read_when_another_page_opened(self) -> None:
        calls = [
            MODULE.ToolCall("read-good", "content_read", True),
            MODULE.ToolCall("read-dead", "content_read", False, error="not_found"),
        ]
        unrecovered, recovered = MODULE.unrecovered_tool_failures(
            calls, answer_supported=True
        )
        self.assertEqual(unrecovered, [])
        self.assertEqual(recovered, 1)

    def test_event_backfill_reads_each_execution_journal_not_cross_scope_history(self) -> None:
        class FakeClient:
            def __init__(self) -> None:
                self.queries = []

            def ndjson(self, path, query):
                self.queries.append((path, query))
                return [{"execution_id": query["execution_id"]}], 2.5

        client = FakeClient()
        events, latency = MODULE.fetch_events(
            client, "task-1", ["exec-1", "exec-2", "exec-1"], 1234
        )
        self.assertEqual(
            [query[1]["execution_id"] for query in client.queries],
            ["exec-1", "exec-2"],
        )
        self.assertTrue(all(query[1]["task_id"] == "task-1" for query in client.queries))
        self.assertTrue(all(query[1]["since"] == 1234 for query in client.queries))
        self.assertEqual([event["execution_id"] for event in events], ["exec-1", "exec-2"])
        self.assertEqual(latency, 5.0)

    def test_ca_bundle_configuration_uses_existing_verified_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "ca.pem"
            bundle.write_text("test", encoding="utf-8")
            with patch.dict(MODULE.os.environ, {}, clear=True):
                self.assertTrue(MODULE.configure_ca_bundle(str(bundle)))
                self.assertEqual(MODULE.os.environ["SSL_CERT_FILE"], str(bundle))
                self.assertFalse(MODULE.configure_ca_bundle(str(bundle)))

    def test_failed_tool_and_llm_values_are_not_silently_truthy(self) -> None:
        events = [
            {
                "event_type": "tool.call.started",
                "call_id": "call-1",
                "tool_name": "content_read",
            },
            {
                "event_type": "tool.call.finished",
                "call_id": "call-1",
                "tool_name": "content_read",
                "success": False,
                "error": "timeout",
            },
        ]
        calls, paired = MODULE.tool_calls_from_events(events)
        self.assertEqual(paired, 1)
        self.assertFalse(calls[0].success)
        self.assertTrue(MODULE.value_is_false("false"))
        self.assertTrue(MODULE.value_is_false(0))

    def test_analytics_busy_contention_retries_but_other_failures_do_not(self) -> None:
        class BusyClient:
            def __init__(self) -> None:
                self.calls = 0

            def json(self, method, path, body=None, query=None, expected=(200,)):
                self.calls += 1
                if self.calls < 3:
                    raise MODULE.EvalFailure(
                        "POST analytics failed with HTTP 503: timed out waiting for "
                        "the analytics DuckDB guard"
                    )
                return {"columns": [], "rows": []}, 4.0, 200

        busy = BusyClient()
        with patch.object(MODULE.time, "sleep") as sleep:
            payload, latency, status = MODULE.analytics_json_with_busy_retry(
                busy, "/analytics", {"sql": "SELECT 1"}
            )
        self.assertEqual(payload, {"columns": [], "rows": []})
        self.assertEqual((latency, status), (4.0, 200))
        self.assertEqual(busy.calls, 3)
        self.assertEqual([call.args[0] for call in sleep.call_args_list], [0.25, 0.5])

        structured = Mock()
        structured.json.side_effect = [
            MODULE.EvalHttpFailure(
                "POST",
                "/analytics",
                503,
                {"code": "analytics_busy", "retryable": True},
            ),
            ({"columns": [], "rows": []}, 3.0, 200),
        ]
        with patch.object(MODULE.time, "sleep") as sleep:
            payload, latency, status = MODULE.analytics_json_with_busy_retry(
                structured, "/analytics", {"sql": "SELECT 1"}
            )
        self.assertEqual((payload, latency, status), ({"columns": [], "rows": []}, 3.0, 200))
        sleep.assert_called_once_with(0.25)

        invalid = Mock()
        invalid.json.side_effect = MODULE.EvalFailure("invalid SQL")
        with self.assertRaisesRegex(MODULE.EvalFailure, "invalid SQL"):
            MODULE.analytics_json_with_busy_retry(
                invalid, "/analytics", {"sql": "not sql"}
            )
        invalid.json.assert_called_once()

    def test_analytics_busy_exhaustion_is_infrastructure_inconclusive(self) -> None:
        busy = Mock()
        busy.json.side_effect = MODULE.EvalHttpFailure(
            "POST",
            "/analytics",
            503,
            {"code": "analytics_busy", "retryable": True},
        )

        with patch.object(MODULE.time, "sleep") as sleep:
            with self.assertRaisesRegex(
                MODULE.AnalyticsInfrastructureInconclusive,
                "remained busy after 4 attempts",
            ):
                MODULE.analytics_json_with_busy_retry(
                    busy, "/analytics", {"sql": "SELECT 1"}
                )

        self.assertEqual(busy.json.call_count, MODULE.ANALYTICS_BUSY_MAX_ATTEMPTS)
        self.assertEqual(
            [call.args[0] for call in sleep.call_args_list],
            [0.25, 0.5, 1.0],
        )

    def test_inconclusive_llm_telemetry_skips_only_telemetry_dependent_gates(self) -> None:
        result = MODULE.CaseResult(
            case_id="case#1",
            fixture_id="case",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            root_status="completed",
            root_execution_id="exec-root",
            execution_ids=["exec-root"],
            execution_agents=["web-researcher"],
            output_id="out-user",
            infrastructure_inconclusive=["llm_telemetry: analytics stayed busy"],
            terminal_lifecycle_events=[
                {
                    "event_type": "agentic.execution.completed",
                    "execution_id": "exec-root",
                }
            ],
        )

        lifecycle = MODULE.lifecycle_gates(result, "A sufficiently short answer")
        lifecycle_names = {gate.name for gate in lifecycle}
        self.assertIn("lifecycle.one_agentic_terminal_per_execution", lifecycle_names)
        self.assertIn("lifecycle.background_operations_nonblocking", lifecycle_names)
        self.assertIn("lifecycle.telemetry_infrastructure_inconclusive", lifecycle_names)
        self.assertNotIn("lifecycle.task_summary_once_per_output_revision", lifecycle_names)
        self.assertNotIn("telemetry.continuation_delta_growth_bounded", lifecycle_names)

        case = {
            "mode": "direct",
            "min_answer_chars": 1,
            "max_answer_words": 100,
            "min_citations": 0,
            "min_distinct_domains": 0,
            "min_resolvable_citations": 0,
            "required_domain_groups": [],
            "required_term_groups": [],
            "required_tool_groups": [],
            "min_paired_tool_calls": 0,
            "max_failed_tool_calls": 0,
            "max_failed_llm_calls": 0,
        }
        result.answer_chars = 10
        result.answer_words = 2
        result.answer_excerpt = "supported answer"
        result.judge_supported = True
        scored = MODULE.score_case(case, result, 0)
        score_names = {gate.name for gate in scored}
        self.assertIn("telemetry.infrastructure_inconclusive", score_names)
        self.assertNotIn("telemetry.linked_llm_calls", score_names)
        self.assertNotIn("telemetry.eval_decision_profile_observed", score_names)

    def test_quality_gates_cover_latency_lineage_sources_tools_and_telemetry(self) -> None:
        case = {
            "mode": "delegated",
            "min_answer_chars": 20,
            "max_answer_words": 80,
            "min_citations": 2,
            "min_distinct_domains": 2,
            "min_resolvable_citations": 1,
            "required_domain_groups": [["python.org"], ["rust-lang.org"]],
            "required_term_groups": [["python"], ["rust"], ["recent"]],
            "required_tool_groups": [["content_search"]],
            "min_paired_tool_calls": 1,
            "max_failed_tool_calls": 0,
            "max_failed_llm_calls": 0,
        }
        result = MODULE.CaseResult(
            case_id="case#1",
            fixture_id="case",
            repeat=1,
            mode="delegated",
            root_agent_id="personal-assistant",
            root_status="completed",
            answer_ready_ms=90_000,
            answer_chars=100,
            answer_words=18,
            answer_excerpt="Python and Rust release comparison: Rust is more recent.",
            citations=[
                "https://www.python.org/downloads/",
                "https://blog.rust-lang.org/releases.html",
            ],
            citation_probes=[
                MODULE.CitationProbe("https://www.python.org/downloads/", 200, 10)
            ],
            judge_supported=True,
            judge_reason="opened pages support the answer",
            tool_calls=[MODULE.ToolCall("c1", "content_search", True, 10)],
            llm_calls=[
                {
                    "success": True,
                    "operation": "agentic_decision",
                    "profile": "gpt6luna-responses-toolsany",
                }
            ],
            execution_agents=["personal-assistant", "web-researcher"],
        )
        gates = MODULE.score_case(case, result, paired_tool_calls=1)
        self.assertTrue(all(gate.passed for gate in gates), [gate for gate in gates if not gate.passed])

    def test_supported_concise_answer_supersedes_minimum_character_heuristic(self) -> None:
        case = {
            "mode": "direct",
            "min_answer_chars": 220,
            "max_answer_words": 80,
            "min_citations": 2,
            "min_distinct_domains": 2,
            "min_resolvable_citations": 2,
            "required_domain_groups": [["openai.com"], ["sarvam.ai"]],
            "required_term_groups": [["gpt-5.6"], ["sarvam"]],
            "required_tool_groups": [["content_search"]],
            "min_paired_tool_calls": 1,
            "max_failed_tool_calls": 0,
            "max_failed_llm_calls": 0,
        }
        result = MODULE.CaseResult(
            case_id="concise#1",
            fixture_id="concise",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            root_status="completed",
            answer_chars=200,
            answer_words=31,
            answer_excerpt="GPT-5.6 and Sarvam prices are compared concisely with exact units.",
            citations=[
                "https://openai.com/api/pricing/",
                "https://www.sarvam.ai/api-pricing",
            ],
            citation_probes=[
                MODULE.CitationProbe("https://openai.com/api/pricing/", 200, 1),
                MODULE.CitationProbe("https://www.sarvam.ai/api-pricing", 200, 1),
            ],
            judge_supported=True,
            judge_reason="all material claims are supported",
            tool_calls=[MODULE.ToolCall("call-1", "content_search", True, 1)],
            llm_calls=[
                {
                    "success": True,
                    "operation": "agentic_decision",
                    "profile": "gpt6luna-responses-toolsany",
                }
            ],
            execution_agents=["web-researcher"],
        )

        gates = MODULE.score_case(case, result, paired_tool_calls=1)
        minimum = next(gate for gate in gates if gate.name == "answer.minimum_content")
        self.assertTrue(minimum.passed)
        self.assertIn("semantic_citation_override=True", minimum.detail)

        result.judge_supported = False
        unsupported_gates = MODULE.score_case(case, result, paired_tool_calls=1)
        unsupported_minimum = next(
            gate for gate in unsupported_gates if gate.name == "answer.minimum_content"
        )
        self.assertFalse(unsupported_minimum.passed)

    def test_slow_answer_is_measured_but_not_failed_by_a_latency_gate(self) -> None:
        case = {
            "mode": "direct",
            "min_answer_chars": 1,
            "max_answer_words": 80,
            "min_citations": 1,
            "min_distinct_domains": 1,
            "min_resolvable_citations": 0,
            "required_domain_groups": [["python.org"]],
            "required_term_groups": [["python"]],
            "min_paired_tool_calls": 0,
            "max_failed_tool_calls": 0,
            "max_failed_llm_calls": 0,
        }
        result = MODULE.CaseResult(
            case_id="slow#1",
            fixture_id="slow",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            root_status="completed",
            answer_ready_ms=120_001,
            answer_chars=6,
            answer_words=1,
            answer_excerpt="Python",
            citations=["https://python.org/"],
            judge_supported=True,
            judge_reason="opened page supports the answer",
            llm_calls=[
                {
                    "success": True,
                    "operation": "agentic_decision",
                    "profile": "gpt6luna-responses-toolsany",
                }
            ],
            execution_agents=["web-researcher"],
        )
        gates = MODULE.score_case(case, result, paired_tool_calls=0)
        self.assertFalse(any(gate.name.startswith("latency.") for gate in gates))
        self.assertTrue(all(gate.passed for gate in gates), gates)

    def test_judge_answer_sends_only_readable_opened_page_text(self) -> None:
        class FakeClient:
            def json(self, method, path, body=None, query=None, expected=(200,)):
                self.request = (method, path, body)
                return {"supported": True, "reason": "all claims are supported"}, 12.0, 200

        client = FakeClient()
        supported, reason, latency, declared_open = MODULE.judge_answer(
            client,
            "Compare prices",
            "The opened pages report the prices.",
            [
                MODULE.CitationProbe(
                    "https://example.test/pricing",
                    200,
                    1,
                    final_url="https://example.test/docs/pricing",
                    title="Pricing",
                    excerpt="Input costs $1 and output costs $2.",
                ),
                MODULE.CitationProbe("https://blocked.test/", 403, 1),
            ],
            "candidate-profile",
        )
        self.assertTrue(supported)
        self.assertEqual(reason, "all claims are supported")
        self.assertEqual(latency, 12.0)
        self.assertEqual(declared_open, [])
        self.assertEqual(client.request[1], "/api/magician/v2/evals/web-researcher/judge")
        self.assertEqual(len(client.request[2]["sources"]), 1)
        self.assertEqual(client.request[2]["llm_profile"], "candidate-profile")
        self.assertEqual(
            client.request[2]["sources"][0]["final_url"],
            "https://example.test/docs/pricing",
        )

    def test_judge_answer_preserves_declared_open_items(self) -> None:
        client = Mock()
        client.json.return_value = {
            "supported": True,
            "reason": "Available prices are supported; the remaining gap is explicit.",
            "declared_open": ["Enterprise pricing requires a quote."],
        }, 12.0, 200
        supported, reason, latency, declared_open = MODULE.judge_answer(
            client,
            "Compare prices",
            "Published prices are listed; enterprise pricing still requires a quote.",
            [MODULE.CitationProbe(
                "https://example.test/pricing", 200, 1,
                excerpt="Published pricing is $1. Contact sales for enterprise pricing.",
            )],
        )
        self.assertTrue(supported)
        self.assertIn("remaining gap is explicit", reason)
        self.assertEqual(latency, 12.0)
        self.assertEqual(declared_open, ["Enterprise pricing requires a quote."])

    def test_operator_stop_marks_case_and_cleans_up_active_task(self) -> None:
        class FakeClient:
            def json(self, method, path, body=None, query=None, expected=(200,)):
                if method == "POST" and path == "/api/magician/v3/tasks":
                    return {
                        "task": {"manifest": {"task_id": "task-1"}}
                    }, 1.0, "application/json"
                if method == "POST" and path.endswith("/execute"):
                    return {
                        "execution": {"state": {"execution_id": "exec-1"}}
                    }, 1.0, "application/json"
                raise AssertionError((method, path))

        case = MODULE.load_cases(
            REPO_ROOT / "scripts/fixtures/web_researcher_live/cases.json"
        )[0]
        client = FakeClient()
        executions = [
            {
                "execution_id": "exec-1",
                "agent_id": "web-researcher",
                "status": "executing",
            }
        ]
        events = [
            {
                "event_type": "tool.call.started",
                "call_id": "call-1",
                "tool_name": "content_search",
            },
            {
                "event_type": "tool.call.finished",
                "call_id": "call-1",
                "tool_name": "content_search",
                "success": True,
            },
        ]
        llm_calls = [
            {
                "operation": "agentic_decision",
                "profile": "gpt6luna-responses-toolsany",
                "success": True,
                "input_tokens": 100,
            }
        ]
        fact_calls = [MODULE.ToolCall("fact-1", "content_search", True, 10)]
        with (
            patch.object(MODULE, "wait_for_answer", side_effect=KeyboardInterrupt),
            patch.object(MODULE, "list_executions", return_value=(executions, 1.0)),
            patch.object(MODULE, "fetch_events", return_value=(events, 1.0)),
            patch.object(
                MODULE, "query_tool_calls", return_value=(fact_calls, 1, 1.0)
            ),
            patch.object(MODULE, "query_llm_calls", return_value=(llm_calls, 1.0)),
            patch.object(MODULE, "cleanup_task", return_value=(True, "removed", 2.0)) as cleanup,
            patch.object(
                MODULE, "cancel_active_execution", return_value=(True, "cancel HTTP 202", 2.0)
            ) as cancel,
        ):
            result, gates = MODULE.run_case(client, case, 1, 0)
        self.assertTrue(result.interrupted)
        self.assertEqual(result.error, "stopped by operator")
        self.assertEqual(result.root_status, "executing")
        self.assertEqual(result.execution_agents, ["web-researcher"])
        self.assertEqual(len(result.tool_calls), 1)
        self.assertEqual(result.tool_calls[0].tool_name, "content_search")
        self.assertEqual(result.llm_calls, llm_calls)
        self.assertTrue(
            any(gate.name == "partial.events_captured" and gate.passed for gate in gates)
        )
        self.assertTrue(
            any(
                gate.name == "partial.tool_facts_captured" and gate.passed
                for gate in gates
            )
        )
        self.assertTrue(
            any(
                gate.name == "partial.telemetry_captured" and gate.passed
                for gate in gates
            )
        )
        self.assertTrue(any(gate.name == "case.operator_stopped" for gate in gates))
        # **Operator stop RETAINS the task and cancels the run.** Deleting it
        # destroyed the decision events, which are the whole reason someone
        # stops a run to look at it. Cancelling is still mandatory: the root was
        # `executing` when the eval unwound, and retention must not mean
        # "left running".
        self.assertTrue(
            any(gate.name == "cleanup.task_retained" and gate.passed for gate in gates)
        )
        self.assertFalse(any(gate.name == "cleanup.task_removed" for gate in gates))
        cleanup.assert_not_called()
        cancel.assert_called_once_with(client, "exec-1")

    def test_cleanup_cancels_active_execution_then_retries_destructive_delete(self) -> None:
        class FakeClient:
            def __init__(self) -> None:
                self.calls: list[tuple[str, str]] = []

            def request(self, method, path, body=None, query=None):
                self.calls.append((method, path))
                if len(self.calls) == 1:
                    return 400, b'{"error":"task_active: running"}', 1.0, "application/json"
                if len(self.calls) == 2:
                    return 200, b'{"cancelled":true}', 2.0, "application/json"
                return 200, b'{"files_removed":true}', 3.0, "application/json"

        client = FakeClient()
        removed, detail, latency = MODULE.cleanup_task(client, "task-1", "exec-1")
        self.assertTrue(removed, detail)
        self.assertEqual(latency, 6.0)
        self.assertEqual(
            client.calls,
            [
                ("DELETE", "/api/magician/v3/tasks/task-1"),
                ("POST", "/api/magician/v3/executions/exec-1/cancel"),
                ("DELETE", "/api/magician/v3/tasks/task-1"),
            ],
        )

    def test_cleanup_retries_transient_directory_not_empty(self) -> None:
        class FakeClient:
            def __init__(self) -> None:
                self.calls = 0

            def request(self, method, path, body=None, query=None):
                self.calls += 1
                if self.calls == 1:
                    return 500, b'{"error":"Directory not empty (os error 66)"}', 1.0, "application/json"
                return 200, b'{"files_removed":true}', 2.0, "application/json"

        client = FakeClient()
        removed, detail, latency = MODULE.cleanup_task(client, "task-1")
        self.assertTrue(removed, detail)
        self.assertEqual(client.calls, 2)
        self.assertEqual(latency, 3.0)

    def test_continuation_growth_tolerates_tokenizer_noise_but_not_unmarked_growth(self) -> None:
        result = MODULE.CaseResult(
            case_id="continuation#1",
            fixture_id="continuation",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            llm_calls=[
                {"operation": "agentic_decision", "execution_id": "exec-1", "input_tokens": 14136, "success": True},
                {"operation": "agentic_decision", "execution_id": "exec-1", "input_tokens": 22637, "success": True},
            ],
        )
        self.assertTrue(MODULE.continuation_growth_gate(result).passed)
        result.llm_calls[1]["input_tokens"] = 24000
        self.assertFalse(MODULE.continuation_growth_gate(result).passed)

    def test_continuation_growth_exempts_explicit_rebootstrap_telemetry(self) -> None:
        result = MODULE.CaseResult(
            case_id="rebootstrap#1",
            fixture_id="rebootstrap",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            llm_calls=[
                {
                    "operation": "agentic_decision",
                    "execution_id": "exec-1",
                    "iteration_id": "exec-1:step:5",
                    "prompt_projection_mode": "continuation",
                    "input_tokens": 12000,
                    "success": True,
                },
                {
                    "operation": "agentic_decision",
                    "execution_id": "exec-1",
                    "iteration_id": "exec-1:step:6",
                    "prompt_projection_mode": "rebootstrap",
                    "input_tokens": 42000,
                    "success": True,
                },
            ],
        )
        self.assertTrue(MODULE.continuation_growth_gate(result).passed)

    def test_seventh_call_is_not_exempt_without_rebootstrap_telemetry(self) -> None:
        calls = [
            {
                "operation": "agentic_decision",
                "execution_id": "exec-1",
                "iteration_id": f"exec-1:step:{index}:prompt_mode=continuation",
                "input_tokens": 12000,
                "success": True,
            }
            for index in range(1, 8)
        ]
        calls[-1]["input_tokens"] = 42000
        result = MODULE.CaseResult(
            case_id="ordinal-is-not-telemetry#1",
            fixture_id="ordinal-is-not-telemetry",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            llm_calls=calls,
        )
        self.assertFalse(MODULE.continuation_growth_gate(result).passed)

    def test_report_contains_case_evidence_and_json_link(self) -> None:
        result = MODULE.CaseResult(
            case_id="self#1",
            fixture_id="self",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            root_status="completed",
            answer_ready_ms=1000,
            answer_chars=20,
            answer_words=3,
            answer_excerpt="A concise cited answer",
            citations=["https://python.org/"],
        )
        gates = [MODULE.Gate("self.pass", True, "ok", "self#1")]
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            payload = MODULE.write_report(
                output, "self-test", Path("cases.json"), [result], gates
            )
            html = (output / "report.html").read_text(encoding="utf-8")
            self.assertTrue(payload["passed"])
            self.assertIn("A concise cited answer", html)
            self.assertIn("report.json", html)

    def test_report_uses_explicit_inconclusive_status_for_analytics_contention(self) -> None:
        result = MODULE.CaseResult(
            case_id="research#1",
            fixture_id="research",
            repeat=1,
            mode="direct",
            root_agent_id="web-researcher",
            root_status="completed",
            infrastructure_inconclusive=["llm_telemetry: analytics stayed busy"],
        )
        gates = [MODULE.Gate("answer.supported", True, "ok", "research#1")]

        payload = MODULE.report_payload("live", Path("cases.json"), [result], gates)

        self.assertEqual(payload["status"], "inconclusive")
        self.assertFalse(payload["passed"])
        self.assertFalse(payload["conclusive"])
        self.assertEqual(payload["summary"]["quality_passed_cases"], 1)
        self.assertEqual(payload["summary"]["passed_cases"], 0)
        self.assertEqual(payload["summary"]["infrastructure_inconclusive_cases"], 1)

        failed = MODULE.report_payload(
            "live",
            Path("cases.json"),
            [result],
            [MODULE.Gate("answer.unsupported", False, "bad claim", "research#1")],
        )
        self.assertEqual(failed["status"], "fail")
        self.assertFalse(failed["passed"])

    def test_make_annotations_use_exact_leaf_report_directories(self) -> None:
        makefile = (REPO_ROOT / "Makefile").read_text(encoding="utf-8")
        self.assertIn(
            "## eval: kind=harness report=evals/web-researcher/deterministic/latest\n"
            "## desc: Provider-free web-research task, lineage, citation, latency, and report regressions\n"
            "test-web-researcher-eval-harness:",
            makefile,
        )
        self.assertIn(
            "## eval: kind=live requires=magician,provider_keys report=evals/web-researcher/live/latest\n"
            "## desc: Real web-researcher direct/delegated answers with observed latency and operator cancellation\n"
            "test-web-researcher-live-eval:",
            makefile,
        )

    def test_shipped_sleuth_contract_keeps_unified_vector_tools_and_bounded_luna_reasoning(self) -> None:
        definition = (
            REPO_ROOT
            / "magician_data_v3/system/agent_templates/agents/web-researcher/definition.agent.yaml"
        ).read_text(encoding="utf-8")
        self.assertIn("- content_search", definition)
        self.assertIn("- content_read", definition)
        self.assertIn("- web_fetch", definition)
        self.assertIn("- working_set_search", definition)
        self.assertIn("- working_set_read", definition)
        self.assertIn("- research-working-sets", definition)
        self.assertIn("agentic_decision:\n      profile: gpt6luna-responses-toolsany", definition)
        self.assertIn("Use one server-owned path for ordinary research", definition)
        self.assertIn('"common":{"fresh":true,"limit":5}', definition)
        self.assertIn("source-access failure", definition)
        self.assertIn("A failed source is an internal attempt", definition)
        self.assertIn("fully closes the evidence gap", definition)
        self.assertIn("Before yielding, reconcile every material final claim", definition)
        self.assertEqual(
            MODULE.declared_agent_tools(definition),
            set(MODULE.EXPECTED_WEB_RESEARCHER_TOOLS),
        )
        self.assertFalse(
            MODULE.declared_agent_tools(definition)
            & set(MODULE.RETIRED_WEB_RESEARCHER_TOOLS)
        )
        self.assertTrue(MODULE.bounded_research_prompt_contract(definition))
        self.assertNotIn("Spend ~50% on reading full articles", definition)
        self.assertNotIn("Structure your final answer as:", definition)
        self.assertNotIn("confirmed across three sources is a finding", definition)

    def test_runtime_preflight_rejects_stale_scope_before_spending(self) -> None:
        class FakeClient:
            def __init__(self, definition):
                self.definition = definition

            def json(self, method, path, body=None, query=None, expected=(200,)):
                self.request = (method, path)
                return {"definition": self.definition}, 2.0, 200

        current = {
            "agent_id": "web-researcher",
            "persona": (
                "The default is bounded research. Start reads at `depth: gist`. "
                "Keep queries normally 2-6 essential words. Search snippets choose pages. "
                "A fetch_status: complete is transport only. Never combine a factual value from a search snippet. "
                "A failed source is an internal attempt. Another opened source fully closes the evidence gap. "
                "Before yielding, reconcile every material final claim and remove each unsupported recommendation or ranking. "
                "Synthesize as soon as evidence is sufficient; do not inflate a "
                "bounded answer into a report. Never add sources solely to hit a count. "
                "Canonical vector: {\"common\":{\"fresh\":true,\"limit\":5}}"
            ),
            "tools": sorted(MODULE.EXPECTED_WEB_RESEARCHER_TOOLS),
            "llm_routing": {
                "operations": {
                    "agentic_decision": {"profile": "gpt6luna-responses-toolsany"}
                }
            },
        }
        self.assertTrue(
            all(gate.passed for gate in MODULE.validate_runtime_agent_contract(FakeClient(current)))
        )

        stale = {
            "agent_id": "web-researcher",
            "persona": (
                "Spend ~50% on reading full articles. "
                "Structure your final answer as: a report."
            ),
            "tools": ["content_search", "content_read", "content_search_batch"],
            "llm_routing": None,
        }
        gates = MODULE.validate_runtime_agent_contract(FakeClient(stale))
        self.assertFalse(all(gate.passed for gate in gates))
        self.assertTrue(
            any(
                gate.name == "runtime_contract.bounded_depth_prompt" and not gate.passed
                for gate in gates
            )
        )
        self.assertTrue(
            any(
                gate.name == "runtime_contract.bounded_luna_decision_profile"
                and not gate.passed
                for gate in gates
            )
        )
        self.assertTrue(
            any(
                gate.name == "runtime_contract.retired_tools_absent"
                and not gate.passed
                for gate in gates
            )
        )

    def test_aggregate_runners_include_harness_and_live_child_report(self) -> None:
        normal = (REPO_ROOT / "scripts/run-all-tests-with-report.sh").read_text(
            encoding="utf-8"
        )
        live = (REPO_ROOT / "scripts/run-live-evals-with-report.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("test-web-researcher-eval-harness", normal)
        self.assertIn('run_eval "Web researcher" "web-researcher"', live)
        self.assertIn('"$web_researcher_status"', live)
        self.assertIn('--suite "Web researcher"', live)
        self.assertIn("web-researcher", live[live.index('case "$only_eval" in'):live.index("esac", live.index('case "$only_eval" in'))])


if __name__ == "__main__":
    unittest.main()
